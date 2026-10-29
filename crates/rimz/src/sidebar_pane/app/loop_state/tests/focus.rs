//! Focus anchors resolving into selection and scroll, the layout freezes that
//! keep the frame still while the user acts, and the read-mark receipts.

use super::*;
use crate::mux::tmux::ControlLine;
use crate::sidebar::presence::{projector::project_presence, tmux::TmuxPresenceState};
use crate::sidebar_pane::app::fixtures::focus_fixture;

fn sampled_focus_rig(mux: MuxName) -> (Rig, PaneId, PaneId, PaneId) {
    let id = |index| {
        PaneId::from_parts(
            mux,
            match mux {
                MuxName::Tmux => format!("%{index}"),
                MuxName::Zellij => format!("terminal_{index}"),
            },
        )
    };
    let (own, p, q) = (id(10), id(11), id(12));
    let clock = LoopClock::play(Timestamp::now());
    let mut rig = Rig::with_clock(Some(own.clone()), BodyLens::default(), None, clock);
    let mut working = vec![pane(p.raw(), "tab_1", false), pane(q.raw(), "tab_1", false)];
    working[0].pane_id = p.clone();
    working[1].pane_id = q.clone();
    let mut snapshot = snapshot_with_panes(&rig.ws, working.clone());
    snapshot.focused_pane = Some(p.clone());
    snapshot.viewed_panes = vec![p.clone()];
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: 2,
        working_pane_ids: vec![p.clone(), q.clone()],
        own_view_is_daemon: false,
    });
    let mut sidebar = pane(own.raw(), "tab_1", false);
    sidebar.pane_id = own.clone();
    sidebar.command = Some("rimz-sidebar".into());
    working.push(sidebar);
    let mut frame =
        crate::sidebar::frame::assemble_frame(working, rig.state.pane.clock.now_ms(), "rimz-test");
    frame.client_views = vec![crate::pane::ClientPaneView {
        client_id: match mux {
            MuxName::Tmux => crate::ids::MuxClientId::Tmux("client-1".into()),
            MuxName::Zellij => crate::ids::MuxClientId::Zellij(1),
        },
        pane_id: p.clone(),
    }];
    frame.client_view_fresh = true;
    deliver_sampled(&mut rig, snapshot, frame);
    (rig, own, p, q)
}

/// Deliver `frame` as the plane's next fold, so the client sample it carries is applied.
fn deliver_sampled(
    rig: &mut Rig,
    snapshot: SidebarSnapshot,
    frame: crate::sidebar::frame::PaneFrame,
) {
    rig.deliver(FetchUpdate::Fold {
        fold: Arc::new(WorkspaceFold {
            sampled_at: Instant::now(),
            workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
            frame: Some(Arc::new(frame)),
        }),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });
    assert!(
        rig.views.lock().has_sample(),
        "the fold's client sample applied"
    );
}

fn focus_event_advances_selection(mux: MuxName) {
    let (mut rig, _, p, q) = sampled_focus_rig(mux);
    assert!(rig.state.observed_watched());
    assert!(rig.next_request().is_none());
    assert_eq!(
        rig.state.inspect_view(|ui, _| ui.selected_pane.clone()),
        Some(p.clone())
    );
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![q.clone()],
        unfocused: vec![p],
    });
    assert_eq!(
        rig.state.inspect_view(|ui, _| ui.selected_pane.clone()),
        Some(q),
        "{mux:?}"
    );
    assert!(rig.state.pane.optimistic_watch_until.is_some());
    assert!(
        rig.next_request().is_none(),
        "{mux:?}: focus within a watched tab needs no fresh production"
    );
}

#[test]
fn tmux_focus_event_advances_selection_before_the_next_client_sample() {
    focus_event_advances_selection(MuxName::Tmux);
}

#[test]
fn zellij_focus_event_advances_selection_before_the_next_client_sample() {
    focus_event_advances_selection(MuxName::Zellij);
}

#[test]
fn zellij_unattributable_focus_event_keeps_the_sample() {
    let (mut rig, _, p, _) = sampled_focus_rig(MuxName::Zellij);
    let fold = rig.views.lock().fold.clone().unwrap();
    let frame = fold.frame.as_ref().unwrap();
    let foreign = zellij("terminal_50");
    let mut panes: Vec<_> = frame
        .tabs
        .iter()
        .flat_map(|tab| {
            tab.panes.iter().map(|entry| {
                let mut pane = pane(entry.pane_id.raw(), "tab_1", false);
                pane.pane_id = entry.pane_id.clone();
                pane.command = entry.current.command.clone();
                pane
            })
        })
        .collect();
    panes.push(pane(foreign.raw(), "tab_2", false));
    let previous = fold.workspace.snapshot();
    let working = panes
        .iter()
        .filter(|pane| pane.command.as_deref() != Some("rimz-sidebar"))
        .cloned()
        .collect();
    let mut snapshot = snapshot_with_panes(&rig.ws, working);
    snapshot.focused_pane = previous.focused_pane.clone();
    snapshot.viewed_panes = previous.viewed_panes.clone();
    snapshot.own_view = previous.own_view.clone();
    let mut sidebar = pane("sidebar_2", "tab_2", false);
    sidebar.command = Some("rimz-sidebar".into());
    panes.push(sidebar);
    let mut next =
        crate::sidebar::frame::assemble_frame(panes, rig.state.pane.clock.now_ms(), "rimz-test");
    next.client_views = frame.client_views.clone();
    next.client_views.push(crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(2),
        pane_id: foreign.clone(),
    });
    next.client_view_fresh = true;
    deliver_sampled(&mut rig, snapshot, next);
    assert_eq!(
        rig.state.inspect_view(|ui, _| ui.selected_pane.clone()),
        Some(p.clone())
    );
    assert!(
        rig.state
            .inspect_view(|_, view| view.current.row_owning_pane(&foreign).is_some()),
        "the foreign client's pane has a row the selection could move to"
    );
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![foreign],
        unfocused: vec![],
    });
    assert_eq!(
        rig.state.inspect_view(|ui, _| ui.selected_pane.clone()),
        Some(p)
    );
}

#[test]
fn focus_failure_follows_its_source_view_after_rerouting() {
    let (mut rig, _, [first, _]) = child_focus_rig();
    let own = rig.state.pane.config.own_pane.clone();
    let other = zellij("sidebar_other");
    let sample = |rig: &Rig, swapped| {
        rig.views.lock().reconcile(
            &[
                crate::pane::ClientPaneView {
                    client_id: crate::ids::MuxClientId::Zellij(1),
                    pane_id: if swapped { other.clone() } else { own.clone() },
                },
                crate::pane::ClientPaneView {
                    client_id: crate::ids::MuxClientId::Zellij(2),
                    pane_id: if swapped { own.clone() } else { other.clone() },
                },
            ],
            true,
            rig.state.pane.clock.now(),
            Duration::from_secs(60),
        );
    };
    sample(&rig, false);
    let source = rig
        .views
        .lock()
        .presentation(&own, rig.state.pane.clock.now(), false)
        .0;
    click_child(&mut rig, &first);
    sample(&rig, true);
    rig.state
        .on_focus_failed(first, "target disappeared".into());
    let mut views = rig.views.lock();
    let source = views.retained(source).unwrap();
    assert!(
        source.pending_input_focus.is_none(),
        "the failed jump no longer keeps the source view's input focus pending"
    );
    assert_eq!(
        source
            .health
            .alert
            .as_ref()
            .map(|alert| alert.reason.as_str()),
        Some("focus failed: parent-at-click: target disappeared")
    );
}

fn child_focus_rig() -> (Rig, PaneId, [PaneId; 2]) {
    let mut rig = Rig::new();
    let baseline = zellij("terminal_1");
    let children = [zellij("terminal_21"), zellij("terminal_22")];
    let mut snapshot = snapshot_with_focused_pane(&rig.ws, baseline.clone());
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[1];
    row.name = "parent-at-click".into();
    row.card = crate::store::snapshot::RowCard::Agent(std::sync::Arc::default());
    let card = row.as_agent_mut().unwrap();
    card.status = crate::agents::AgentStatus::Running;
    card.user_turn_started_at = Some(snapshot.now);
    card.sub_agent_count = 2;
    card.sub_agents = children
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            serde_json::from_value(serde_json::json!({
                "id": format!("child-{index}"), "kind": "claude", "name": format!("child-{index}"),
                "status": "running", "last_activity": snapshot.now, "pane": pane,
            }))
            .unwrap()
        })
        .collect();
    rig.view()
        .ui()
        .delegation_overrides
        .insert(row.id.clone(), true);
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    (rig, baseline, children)
}

fn click_child(rig: &mut Rig, child: &PaneId) {
    rig.state.pane.click_guard = None;
    let (column, row) = rig
        .state
        .pane
        .ui
        .interactions
        .line_for_target(&render::HitTarget::SubAgentPane(child.clone()))
        .expect("painted child entry");
    let applied = rig
        .state
        .apply_input([Wakeup::MouseClick { column, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(applied.effects, vec![InputEffect::Focus(child.clone())]);
}

#[test]
fn failed_child_jump_restores_the_cursor_derived_from_the_original_child_focus() {
    let (mut rig, _, [original, target]) = child_focus_rig();
    let mut snapshot = rig.state.views.mirror().clone();
    snapshot.focused_pane = Some(original.clone());
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    click_child(&mut rig, &target);
    rig.state
        .on_wakeup(
            &mut rig.terminal,
            Wakeup::FocusFailed {
                pane: target,
                error: "target disappeared".to_owned(),
            },
        )
        .unwrap();
    let entered = rig
        .state
        .apply_input([Wakeup::Key(KeyAction::Enter)], &mut rig.terminal)
        .unwrap();
    assert_eq!(entered.effects, vec![InputEffect::Focus(original)]);
}

fn tall_child_focus_rig() -> (Rig, Vec<PaneId>) {
    let (mut rig, _, [first, _]) = child_focus_rig();
    let mut snapshot = rig.state.views.mirror().clone();
    snapshot.focused_pane = Some(first.clone());
    let parent_id = snapshot.row_owning_pane(&first).unwrap().id.clone();
    let row = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )
    .iter_mut()
    .find(|row| row.id == parent_id)
    .unwrap();
    let card = row.as_agent_mut().unwrap();
    let mut child = card.sub_agents[0].clone();
    child.model = Some("Haiku".to_owned());
    let panes: Vec<_> = (21..41)
        .map(|id| zellij(&format!("terminal_{id}")))
        .collect();
    card.sub_agents = panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            let mut child = child.clone();
            child.id = format!("child-{index}");
            child.pane = Some(pane.clone());
            child
        })
        .collect();
    card.sub_agent_count = panes.len() as u32;
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    click_child(&mut rig, &first);
    (rig, panes)
}

#[test]
fn keyboard_entry_steps_redraw_and_reveal_both_lines_inside_a_tall_card() {
    let (mut rig, panes) = tall_child_focus_rig();
    for pane in panes.iter().skip(1) {
        let offset = rig.view().ui().scroll_offset;
        let applied = rig
            .state
            .apply_input([Wakeup::Key(KeyAction::Down)], &mut rig.terminal)
            .unwrap();
        assert!(
            applied.redraw,
            "an entry step redraws even though the card is unchanged"
        );
        assert!(applied.effects.is_empty(), "a step never focuses");
        let visible = rig
            .state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::SubAgentPane(pane.clone()));
        assert!(visible.is_some(), "stepped entry {pane} must be visible");
        let (column, line) = visible.unwrap();
        assert_eq!(
            rig.state.pane.ui.interactions.target_at(column, line + 1),
            Some(render::HitTarget::SubAgentPane(pane.clone()))
        );
        let next = rig.view().ui().scroll_offset;
        assert!(
            next.saturating_sub(offset) <= 2,
            "minimal reveal: {offset} -> {next}"
        );
    }
}

#[test]
fn raw_entry_steps_reveal_both_lines_despite_an_authoritative_offset() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    for code in [KeyCode::Char('j'), KeyCode::Down] {
        let (mut rig, panes) = tall_child_focus_rig();
        for pane in panes.iter().skip(1) {
            rig.view().ui().offset_authoritative = true;
            let applied = rig
                .state
                .apply_input(
                    [Wakeup::Press {
                        code,
                        mods: KeyModifiers::NONE,
                    }],
                    &mut rig.terminal,
                )
                .unwrap();
            assert!(applied.redraw);
            let target = render::HitTarget::SubAgentPane(pane.clone());
            let visible = rig.state.pane.ui.interactions.line_for_target(&target);
            assert!(
                visible.is_some(),
                "{code:?}: stepped entry {pane} must show"
            );
            let (column, line) = visible.unwrap();
            assert_eq!(
                rig.state.pane.ui.interactions.target_at(column, line + 1),
                Some(target),
                "{code:?}: both entry lines must show"
            );
        }
    }
}

#[test]
fn delegation_header_focuses_parent_while_a_child_pane_is_focused() {
    let (mut rig, _, [child, _]) = child_focus_rig();
    let mut snapshot = rig.state.views.mirror().clone();
    let parent_row = snapshot.row_owning_pane(&child).unwrap().clone();
    snapshot.focused_pane = Some(child);
    let parent = parent_row.pane.as_ref().unwrap().pane_id.clone();
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    rig.state.pane.click_guard = None;
    let (column, row) = rig
        .state
        .pane
        .ui
        .interactions
        .line_for_target(&render::HitTarget::ToggleDelegation {
            row: parent_row.id,
            open: false,
        })
        .unwrap();
    let applied = rig
        .state
        .apply_input([Wakeup::MouseClick { column, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(applied.effects, vec![InputEffect::Focus(parent.clone())]);
    assert_eq!(rig.view().ui().selected_pane, Some(parent));
}

#[test]
fn stale_sibling_focus_failure_preserves_the_latest_pick_and_rollback() {
    let (mut rig, baseline, [first, second]) = child_focus_rig();
    click_child(&mut rig, &first);
    click_child(&mut rig, &second);
    let selected = rig.view().ui().selected_pane.clone();
    let browse = rig.view().ui().browse.clone();
    rig.view().ui().focus_placement = Some(crate::mux::focus_anchor::FocusPlacement {
        row_id: selected.as_ref().unwrap().to_string(),
        viewport_row: 2,
        held: Default::default(),
    });
    let placement = rig.view().ui().focus_placement.clone();
    rig.state.on_focus_failed(first, "stale sibling".into());
    assert_eq!(
        rig.view().ui().browse,
        browse,
        "older failure cannot clear the newer browse"
    );
    assert_eq!(rig.view().ui().selected_pane, selected);
    assert_eq!(rig.view().ui().focus_placement, placement);
    assert!(rig.view().ctx().health.alert.is_none());
    rig.state.on_focus_failed(second, "latest failure".into());
    assert_eq!(rig.view().ui().baseline_pane, Some(baseline));
    assert!(rig.view().ui().browse.is_none());
    assert_eq!(
        rig.view().ctx().health.alert.as_ref().unwrap().reason,
        "focus failed: parent-at-click: latest failure"
    );
}

#[test]
fn failed_child_focus_after_card_departure_uses_the_clicked_label() {
    let (mut rig, baseline, [first, _]) = child_focus_rig();
    click_child(&mut rig, &first);
    let parent = rig.view().ui().selected_pane.clone().unwrap();
    rig.event(SidebarEvent::PaneClosed { pane_id: parent });
    assert!(rig.view().ctx().current.row_owning_pane(&first).is_none());
    rig.state
        .on_focus_failed(first, "target disappeared".into());
    assert_eq!(rig.view().ui().baseline_pane, Some(baseline));
    assert!(rig.view().ui().browse.is_none());
    assert_eq!(
        rig.view()
            .ctx()
            .health
            .alert
            .as_ref()
            .expect("departure still reports focus failure")
            .reason,
        "focus failed: parent-at-click: target disappeared"
    );
}

fn picked_focus_rig() -> (Rig, PaneId, PaneId) {
    let mut rig = Rig::new();
    let baseline = zellij("terminal_1");
    let picked = zellij("terminal_2");
    rig.fold(
        snapshot_with_focused_pane(&rig.ws, baseline.clone()),
        SnapshotSource::Produced,
    );
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    rig.state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.view().ui().drawn_selection, Some(picked.to_string()));
    (rig, baseline, picked)
}

#[test]
fn focus_failure_rolls_back_and_paints_a_recovered_notice_even_after_intent_adoption() {
    for adopted in [false, true] {
        let (mut rig, baseline, picked) = picked_focus_rig();
        if adopted {
            let anchor =
                requested_focus_anchor(picked.clone(), 2, crate::utils::time::unix_now_ms(), None);
            crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
            rig.state.fold_focus_intent(anchor.clone());
            assert!(
                rig.view().ui().browse.is_none(),
                "intent adoption already ended browsing"
            );
            assert_eq!(rig.view().ui().baseline_pane, Some(picked.clone()));
            rig.view().ctx().fold_inputs = Some(Arc::new(super::super::super::fetch::FoldInputs {
                sampled_at: Instant::now(),
                anchor: Some(anchor.clone()),
                filter: rig.state.inspect_view(|ui, _| ui.make_up_filter.clone()),
            }));
            assert!(crate::mux::focus_anchor::clear_matching(
                &rig.runtime,
                anchor.nonce
            ));
        }
        rig.state
            .on_wakeup(
                &mut rig.terminal,
                Wakeup::FocusFailed {
                    pane: picked,
                    error: "target disappeared".into(),
                },
            )
            .unwrap();
        rig.state
            .paint_now(&mut rig.terminal, Instant::now())
            .unwrap();
        assert_eq!(
            rig.view().ui().drawn_selection,
            Some(baseline.to_string()),
            "failed focus restores the pulled baseline"
        );
        assert!(rig.view().ui().browse.is_none());
        let alert = rig
            .view()
            .ctx()
            .health
            .alert
            .clone()
            .expect("focus failure notice");
        assert_eq!(alert.reason, "focus failed: zsh: target disappeared");
        assert!(!alert.is_active());
        let theme = {
            let pane = &mut rig.state.pane;
            let mut guard = rig.state.views.rig(&pane.config.own_pane, pane.clock.now());
            let (_, context) = guard.pair();
            pane.ui.theme(&context.current.theme)
        };
        let frame = {
            let pane = &mut rig.state.pane;
            let mut guard = rig.state.views.rig(&pane.config.own_pane, pane.clock.now());
            let (ui, context) = guard.pair();
            render::compose_lines(&context.current, Some(&alert), ui, &pane.ui, &theme, 80, 24)
        };
        assert!(frame.lines.iter().any(|line| {
            line.to_string()
                .contains("focus failed: zsh: target disappeared")
        }));
    }
    let (mut rig, baseline, picked) = picked_focus_rig();
    rig.state
        .apply_input([Wakeup::Key(KeyAction::Up)], &mut rig.terminal)
        .unwrap();
    rig.state
        .on_wakeup(
            &mut rig.terminal,
            Wakeup::FocusFailed {
                pane: picked,
                error: "late failure".into(),
            },
        )
        .unwrap();
    assert_eq!(rig.view().ui().selected_pane, Some(baseline));
    assert!(rig.view().ctx().health.alert.is_none());
}

#[test]
fn focus_worker_reports_request_and_dispatch_errors_but_not_superseded_dispatch() {
    let rig = Rig::new();
    let (snapshot, _, _, _) = focus_fixture();
    let expected = snapshot.client_views;
    let mut moved = expected.clone();
    moved[0].pane_id = zellij("terminal_77");
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    let target = zellij("terminal_9");
    let plane = super::super::super::plane::DataPlane::detached();
    let peers = plane.focus_peers(
        rig.state.pane.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    );
    let lease = peers.arm(target.clone(), Instant::now());
    let requested = Some(fenced_request(&rig, &expected, moved));
    finish_focus_request(&target, Some(&tx), requested, Some(&lease), |_| {
        panic!("failed request must not dispatch")
    });
    assert!(
        matches!(rx.try_recv(), Ok(Wakeup::FocusFailed { pane, error }) if pane == target && !error.is_empty())
    );
    assert!(
        !lease.is_current(),
        "rollback consumer must see the lease released"
    );
    for dispatched in [Err(FocusActionError::Superseded), Ok(false), Ok(true)] {
        let failed = dispatched.is_err();
        let requested = Some(fenced_request(&rig, &expected, expected.clone()));
        let lease = peers.arm(target.clone(), Instant::now());
        finish_focus_request(&target, Some(&tx), requested, Some(&lease), |_| {
            assert!(lease.is_current(), "lease lasts through dispatch");
            dispatched
        });
        assert!(!lease.is_current(), "dispatch return releases the lease");
        if failed {
            assert!(
                matches!(rx.try_recv(), Ok(Wakeup::FocusFailed { pane, error }) if pane == target && !error.is_empty())
            );
        } else {
            assert!(
                rx.try_recv().is_err(),
                "superseded or successful dispatch is not a failure"
            );
        }
    }
    let lease = peers.arm(target.clone(), Instant::now());
    finish_focus_request(&target, None, None, Some(&lease), |_| unreachable!());
    assert!(!lease.is_current());
}

fn assert_failed_dispatch_restores_baseline(mux: MuxName) {
    use crate::mux::focus_anchor::{
        FocusActionRequest, dispatch_action, request_action_with_test_sample,
    };

    let (own, baseline, picked, client_id) = match mux {
        MuxName::Zellij => (
            zellij("terminal_10"),
            zellij("terminal_1"),
            zellij("terminal_2"),
            crate::ids::MuxClientId::Zellij(1),
        ),
        MuxName::Tmux => (
            tmux("%10"),
            tmux("%1"),
            tmux("%2"),
            crate::ids::MuxClientId::Tmux("test-client".to_owned()),
        ),
    };
    let mut rig = Rig::with_own_pane(own.clone());
    let panes = [baseline.clone(), picked.clone()]
        .into_iter()
        .map(|id| {
            let mut working = pane("terminal_1", "view_1", false);
            working.pane_id = id;
            working.view_kind = Some(match mux {
                MuxName::Zellij => crate::ids::ViewKind::Tab,
                MuxName::Tmux => crate::ids::ViewKind::Window,
            });
            working
        })
        .collect();
    let mut native = snapshot_with_panes(&rig.ws, panes);
    native.presence = Some(crate::store::snapshot::SidebarPresence::Active);
    native.own_view = Some(SidebarOwnView {
        sibling_count: 3,
        working_pane_ids: vec![baseline.clone(), picked.clone()],
        own_view_is_daemon: false,
    });
    native.focused_pane = Some(baseline.clone());
    native.viewed_panes = vec![baseline.clone()];
    native.client_views = vec![crate::pane::ClientPaneView {
        client_id,
        pane_id: baseline.clone(),
    }];
    rig.fold(native.clone(), SnapshotSource::Produced);
    native.focused_pane = Some(own.clone());
    native.viewed_panes = vec![own.clone()];
    native.client_views[0].pane_id = own;
    rig.fold(native.clone(), SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    assert_eq!(rig.view().ui().drawn_selection, Some(baseline.to_string()));
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    let outcome = rig
        .state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert!(
        outcome
            .effects
            .contains(&InputEffect::Focus(picked.clone()))
    );
    assert_eq!(rig.view().ui().drawn_selection, Some(picked.to_string()));

    let nonce = request_action_with_test_sample(
        &rig.runtime,
        "rimz-test",
        FocusActionRequest {
            pane_id: picked.clone(),
            origin: FocusOrigin::User,
            repair_generation: None,
            expected_pre_action: Some(&native.client_views),
            placement: None,
            order: None,
        },
        || Ok(native.client_views.clone()),
    )
    .unwrap();
    let anchor = crate::mux::focus_anchor::load(&rig.runtime).unwrap();
    rig.state.fold_focus_intent(anchor.clone());
    assert!(
        rig.view().ui().browse.is_none(),
        "request adoption ended browsing before dispatch"
    );
    assert_eq!(rig.view().ui().baseline_pane, Some(picked.clone()));
    rig.view().ctx().fold_inputs = Some(Arc::new(super::super::super::fetch::FoldInputs {
        sampled_at: Instant::now(),
        anchor: Some(anchor),
        filter: rig.state.inspect_view(|ui, _| ui.make_up_filter.clone()),
    }));

    // A backend mismatch fails before spawning, but runs real dispatch-error cleanup.
    let rejecting_backend = crate::mux::backend_for(match mux {
        MuxName::Zellij => MuxName::Tmux,
        MuxName::Tmux => MuxName::Zellij,
    });
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    finish_focus_request(&picked, Some(&tx), Some(Ok(nonce)), None, |nonce| {
        dispatch_action(
            rejecting_backend.as_ref(),
            &rig.runtime,
            "rimz-test",
            &picked,
            nonce,
            Default::default(),
        )
    });
    assert!(
        crate::mux::focus_anchor::load(&rig.runtime).is_none(),
        "failed dispatch removed its request"
    );
    rig.state
        .on_wakeup(&mut rig.terminal, rx.try_recv().unwrap())
        .unwrap();
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    assert_eq!(
        rig.view().ui().drawn_selection,
        Some(baseline.to_string()),
        "failed dispatch restores the pre-jump working-pane baseline while native focus stays on the sidebar"
    );
    let alert = rig.view().ctx().health.alert.clone().unwrap();
    assert!(alert.reason.starts_with("focus failed: zsh:"));
    assert!(!alert.is_active());
    rig.shared_fold(
        native,
        Arc::new(super::super::super::fetch::FoldInputs {
            sampled_at: Instant::now(),
            anchor: None,
            filter: rig.state.inspect_view(|ui, _| ui.make_up_filter.clone()),
        }),
    );
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    assert_eq!(
        rig.view().ui().drawn_selection,
        Some(baseline.to_string()),
        "the next shared cut must not revive the failed target"
    );
}

#[test]
fn failed_tmux_dispatch_restores_baseline_while_sidebar_is_focused() {
    assert_failed_dispatch_restores_baseline(MuxName::Tmux);
}

#[test]
fn failed_zellij_dispatch_restores_baseline_while_sidebar_is_focused() {
    assert_failed_dispatch_restores_baseline(MuxName::Zellij);
}

/// Rules out the hypothesized late-confirmation A→B→A revert.
#[test]
fn clicked_selection_stays_painted_through_late_native_confirmation_and_fence() {
    let mut rig = Rig::new();
    let baseline = zellij("terminal_1");
    let target = zellij("terminal_2");
    let issued = crate::utils::time::unix_now_ms() - 2_600;
    let mut native = snapshot_with_focused_pane(&rig.ws, baseline.clone());
    native.presence = Some(crate::store::snapshot::SidebarPresence::Active);
    native.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(1),
        pane_id: baseline,
    }];
    native.panes_observed_at_ms = Some(issued - 1);
    rig.fold(native.clone(), SnapshotSource::Produced);
    rig.state.pane.dirty = true;
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    let now = Instant::now();
    rig.state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(
        rig.view().ui().drawn_selection,
        Some(target.to_string()),
        "click paint selects B"
    );
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    rig.state.pane.observe_tx = tx;
    native.panes_observed_at_ms = Some(issued + 500);
    rig.fold(native.clone(), SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, now + Duration::from_millis(500))
        .unwrap();
    assert_eq!(rig.view().ui().drawn_selection, Some(target.to_string()));
    let mut anchor = applied_focus_anchor(target.clone(), 2, issued, None);
    anchor.pre_action = native.client_views.clone();
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    rig.fold(native.clone(), SnapshotSource::Produced);
    assert!(
        rig.view().ctx().current.focused_pane.is_none(),
        "aged anchor fences stale A"
    );
    rig.state
        .paint_now(&mut rig.terminal, now + Duration::from_millis(2_600))
        .unwrap();
    assert_eq!(rig.view().ui().drawn_selection, Some(target.to_string()));
    native.focused_pane = Some(target.clone());
    native.client_views[0].pane_id = target.clone();
    native.panes_observed_at_ms = Some(issued + 3_000);
    rig.fold(native, SnapshotSource::Produced);
    rig.state
        .paint_now(&mut rig.terminal, now + Duration::from_millis(3_000))
        .unwrap();
    assert_eq!(rig.view().ui().drawn_selection, Some(target.to_string()));
    for message in rx.try_iter() {
        if let ObserveMsg::OwnAnomaly { draft, .. } = message {
            assert_ne!(
                serde_json::to_value(draft.kind).unwrap()["cause"],
                "selection_changed"
            );
        }
    }
}

fn tmux(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Tmux, raw)
}

fn tmux_subscription(raw: &str, window: &str, active: bool) -> ControlLine {
    ControlLine::Subscription {
        pane: raw.to_owned(),
        window: window.to_owned(),
        command: Some("zsh".to_owned()),
        active,
        title: None,
        floating: false,
    }
}

fn tmux_focus_snapshot(rig: &Rig, third_window: &str, stamp: u64) -> SidebarSnapshot {
    let panes = [("%1", "@1"), ("%2", "@2"), ("%3", third_window)]
        .into_iter()
        .map(|(raw, window)| {
            let mut result = pane(raw, window, false);
            result.pane_id = tmux(raw);
            result.view_kind = Some(crate::ids::ViewKind::Window);
            result
        })
        .collect();
    let mut result = snapshot_with_panes(&rig.ws, panes);
    result.panes_produced_at_ms = Some(stamp);
    result.panes_observed_at_ms = Some(stamp);
    result.focused_pane = Some(tmux("%1"));
    result.viewed_panes = vec![tmux("%1")];
    result.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Tmux("test-client".to_owned()),
        pane_id: tmux("%1"),
    }];
    result
}

#[test]
fn tmux_sidebar_departure_releases_parked_freeze_before_the_next_pull() {
    let own = tmux("%7");
    let mut producer = TmuxPresenceState::default();
    let mut sidebar = tmux_subscription("%7", "@1", false);
    if let ControlLine::Subscription { title, command, .. } = &mut sidebar {
        *title = Some(crate::pane::SIDEBAR_CHROME_TITLE.to_owned());
        *command = Some("rimz".to_owned());
    }
    for line in [tmux_subscription("%1", "@1", true), sidebar] {
        producer.apply(line, true);
    }
    let mut rig = Rig::with_own_pane(own.clone());
    let now = crate::utils::time::unix_now_ms();
    let mut pulled = tmux_focus_snapshot(&rig, "@1", now - 100);
    pulled.focused_pane = Some(own.clone());
    pulled.client_views[0].pane_id = own.clone();
    rig.fold(pulled, SnapshotSource::Produced);
    for event in project_presence(
        producer
            .apply(
                ControlLine::WindowPaneChanged {
                    window: "@1".to_owned(),
                    pane: "%7".to_owned(),
                },
                false,
            )
            .0,
    ) {
        rig.event(event);
    }
    {
        let pane = &mut rig.state.pane;
        let mut guard = rig.state.views.rig(&pane.config.own_pane, pane.clock.now());
        let (_, context) = guard.pair();
        pane.ui.theme(&context.current.theme);
    }
    layout_freeze::arm_freeze(rig.view().ui(), now as i64 - 6_000);
    assert!(rig.state.parked(now));
    let departure = project_presence(
        producer
            .apply(
                ControlLine::WindowPaneChanged {
                    window: "@1".to_owned(),
                    pane: "%1".to_owned(),
                },
                false,
            )
            .0,
    );
    assert_eq!(
        departure,
        vec![SidebarEvent::FocusChanged {
            focused: vec![tmux("%1")],
            unfocused: vec![],
        }]
    );
    for event in departure {
        rig.event(event);
    }
    assert!(
        rig.view().ui().freeze.is_none(),
        "departure releases in the event fold, not a later pull"
    );
    assert_eq!(
        rig.view().ctx().current.client_views[0].pane_id,
        own,
        "pull still reports the sidebar"
    );
}

fn assert_tmux_activation_selection(third_window: &str, line: ControlLine, expected: &str) {
    let mut producer = TmuxPresenceState::default();
    for line in [
        tmux_subscription("%1", "@1", true),
        tmux_subscription("%2", "@2", true),
        tmux_subscription("%3", third_window, false),
        ControlLine::SeedPane {
            pane: "%1".to_owned(),
            window: "@1".to_owned(),
            x: 0,
            width: 80,
            window_width: 80,
            session: "$1".to_owned(),
            window_active: true,
        },
    ] {
        assert!(producer.apply(line, true).0.is_empty());
    }
    let mut rig = Rig::new();
    let stamp = crate::utils::time::unix_now_ms() - 100;
    rig.fold(
        tmux_focus_snapshot(&rig, third_window, stamp),
        SnapshotSource::Produced,
    );
    assert_eq!(rig.view().ui().selected_pane, Some(tmux("%1")));
    let clients = rig.view().ctx().current.client_views.clone();
    for event in project_presence(producer.apply(line, false).0) {
        let envelope = SidebarEventEnvelope::new(
            rig.ws.clone(),
            Some("rimz-test".to_owned()),
            stamp + 10,
            event,
        );
        rig.record_event(&envelope);
        rig.state.on_event(&mut rig.terminal, envelope);
    }
    assert_eq!(rig.view().ctx().current.client_views, clients);
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(tmux(expected)),
        "interim selection"
    );
    rig.fold(
        tmux_focus_snapshot(&rig, third_window, stamp),
        SnapshotSource::Produced,
    );
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(tmux(expected)),
        "produced/final selection"
    );
    let mut latest = tmux_focus_snapshot(&rig, third_window, stamp + 20);
    if third_window == "@1" {
        latest.focused_pane = Some(tmux("%3"));
        latest.viewed_panes = vec![tmux("%3")];
        latest.client_views[0].pane_id = tmux("%3");
    }
    rig.fold(latest, SnapshotSource::Produced);
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(tmux(expected)),
        "following pull selection"
    );
}

#[test]
fn hidden_window_pane_changed_must_not_select_phantom() {
    assert_tmux_activation_selection(
        "@2",
        ControlLine::WindowPaneChanged {
            window: "@2".to_owned(),
            pane: "%3".to_owned(),
        },
        "%1",
    );
}

#[test]
fn hidden_subscription_activation_must_not_select_phantom() {
    assert_tmux_activation_selection("@2", tmux_subscription("%3", "@2", true), "%1");
}

#[test]
fn current_window_focus_change_selects_and_stays_control() {
    assert_tmux_activation_selection(
        "@1",
        ControlLine::WindowPaneChanged {
            window: "@1".to_owned(),
            pane: "%3".to_owned(),
        },
        "%3",
    );
}

fn fenced_request(
    rig: &Rig,
    expected: &[crate::pane::ClientPaneView],
    sample: Vec<crate::pane::ClientPaneView>,
) -> std::result::Result<crate::ids::FocusNonce, crate::mux::focus_anchor::FocusActionError> {
    use crate::mux::focus_anchor::{FocusActionRequest, request_action_with_test_sample};
    request_action_with_test_sample(
        &rig.runtime,
        "rimz-test",
        FocusActionRequest {
            pane_id: zellij("terminal_9"),
            origin: FocusOrigin::AutomaticRepair,
            repair_generation: Some(3),
            expected_pre_action: Some(expected),
            placement: None,
            order: None,
        },
        || Ok(sample),
    )
}

#[test]
fn focus_request_retries_mismatching_samples_until_the_fence_agrees() {
    let rig = Rig::new();
    let (snapshot, _, _, _) = focus_fixture();
    let expected = snapshot.client_views;
    let mut moved = expected.clone();
    moved[0].pane_id = zellij("terminal_77");
    let now = std::cell::Cell::new(1_000);
    let samples = std::cell::Cell::new(0);
    let outcome = request_focus_until(
        Some(2_000),
        &expected,
        || {
            samples.set(samples.get() + 1);
            let sample = if samples.get() <= 2 {
                &moved
            } else {
                &expected
            };
            fenced_request(&rig, &expected, sample.clone())
        },
        || now.get(),
        |delay| now.set(now.get() + duration_millis(delay)),
    );
    assert!(
        outcome.requested.expect("attempted").is_ok(),
        "one transient fence disagreement must not end repair"
    );
    assert_eq!(outcome.attempts, 3);
    assert_eq!(samples.get(), 3);
    assert_eq!(outcome.observed, expected);
    assert_eq!(now.get(), 1_200);
    assert_eq!(
        crate::mux::focus_anchor::load(&rig.runtime)
            .expect("accepted intent")
            .pane_id,
        zellij("terminal_9")
    );
}

#[test]
fn focus_request_deadline_keeps_the_last_mismatching_sample() {
    let rig = Rig::new();
    let (snapshot, _, _, _) = focus_fixture();
    let expected = snapshot.client_views;
    let now = std::cell::Cell::new(1_000);
    let samples = std::cell::Cell::new(0);
    let sample = |number| {
        let mut view = expected.clone();
        view[0].pane_id = zellij(&format!("terminal_{number}"));
        view
    };
    let outcome = request_focus_until(
        Some(1_250),
        &expected,
        || {
            samples.set(samples.get() + 1);
            fenced_request(&rig, &expected, sample(100 + samples.get()))
        },
        || now.get(),
        |delay| now.set(now.get() + duration_millis(delay)),
    );
    assert_eq!(outcome.attempts, 3);
    assert_eq!(now.get(), 1_250);
    assert_eq!(outcome.observed, sample(103));
    assert!(
        matches!(outcome.requested, Some(Err(crate::mux::focus_anchor::FocusActionError::PreObservationChanged { observed })) if observed == sample(103))
    );
    assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());
}

#[test]
fn expired_focus_request_does_not_sample_or_write_an_intent() {
    let rig = Rig::new();
    let (snapshot, _, _, _) = focus_fixture();
    let expected = snapshot.client_views;
    let samples = std::cell::Cell::new(0);
    let outcome = request_focus_until(
        Some(1_000),
        &expected,
        || {
            samples.set(samples.get() + 1);
            fenced_request(&rig, &expected, expected.clone())
        },
        || 1_000,
        |_| panic!("expired repair must not sleep"),
    );
    assert!(outcome.requested.is_none());
    assert_eq!(outcome.attempts, 0);
    assert_eq!(samples.get(), 0);
    assert!(outcome.observed.is_empty());
    assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());
}

#[test]
fn focus_request_does_not_retry_one_shot_or_other_errors() {
    use crate::mux::focus_anchor::FocusActionError;
    for deadline in [None, Some(2_000)] {
        let outcome = request_focus_until(
            deadline,
            &[],
            || Err(FocusActionError::Superseded),
            || 1_000,
            |_| panic!("non-fence errors must not retry"),
        );
        assert_eq!(outcome.attempts, 1);
        assert!(matches!(
            outcome.requested,
            Some(Err(FocusActionError::Superseded))
        ));
    }
    let outcome = request_focus_until(
        None,
        &[],
        || {
            Err(FocusActionError::PreObservationChanged {
                observed: Vec::new(),
            })
        },
        || 1_000,
        |_| panic!("one-shot requests must not retry"),
    );
    assert_eq!(outcome.attempts, 1);
    assert!(outcome.requested.expect("attempted").is_err());
}

#[test]
fn foreign_strand_has_no_effect_while_own_strand_waits_for_a_working_row() {
    let own = zellij("terminal_0");
    let foreign = zellij("terminal_2");
    let mut rig = Rig::with_own_pane(own.clone());
    crate::diag::focus_repair::take_appends();
    let strand = |pane_id: PaneId, generation| SidebarEvent::FocusStranded {
        clients: vec![crate::pane::ClientPaneView {
            client_id: crate::ids::MuxClientId::Zellij(1),
            pane_id: pane_id.clone(),
        }],
        pane_id,
        generation,
    };

    rig.event(strand(foreign.clone(), 1));
    assert!(rig.state.pane.pending_focus_repair.is_none());
    assert!(rig.next_request().is_none());
    assert_eq!(crate::diag::focus_repair::take_appends(), 0);

    rig.event(strand(own.clone(), 2));
    let pending = rig
        .state
        .pane
        .pending_focus_repair
        .as_ref()
        .expect("own repair");
    assert_eq!(pending.pane_id, own);
    assert_eq!(pending.generation, 2);
    assert!(rig.next_request().expect("fresh panes").is_fresh_panes());
    assert_eq!(crate::diag::focus_repair::take_appends(), 0);

    rig.event(strand(foreign, 3));
    assert_eq!(
        rig.state
            .pane
            .pending_focus_repair
            .as_ref()
            .expect("own repair")
            .generation,
        2,
    );
    assert!(rig.next_request().is_none());
    assert_eq!(crate::diag::focus_repair::take_appends(), 0);
}

#[test]
fn focus_stranded_targets_recent_own_pane_events_only() {
    let (snapshot, sidebar, _first_work, second_work) = focus_fixture();
    let ui = render::ViewState {
        baseline_pane: Some(second_work.clone()),
        ..Default::default()
    };

    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ui,
            &sidebar,
            &snapshot.client_views,
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        Some(second_work.clone()),
    );

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let ui = render::ViewState {
        baseline_pane: Some(second_work.clone()),
        ..Default::default()
    };
    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ui,
            &sidebar,
            &snapshot.client_views,
            &foreign,
            1_000,
            1_050,
        )
        .ok(),
        None,
    );

    let now = 1_000 + duration_millis(FOCUS_STRANDED_EVENT_TTL) + 1;
    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ui,
            &sidebar,
            &snapshot.client_views,
            &sidebar,
            1_000,
            now,
        )
        .ok(),
        None,
    );
}

/// A deferred repair is retried for exactly as long as its evidence can still
/// authorize one: the retry window and `focus_stranded_target`'s freshness
/// bound are the same TTL, so a repair is never dropped while it would still be
/// honored, nor retried once it would be refused.
#[test]
fn focus_repair_retry_window_matches_the_strand_event_ttl() {
    let ttl = duration_millis(FOCUS_STRANDED_EVENT_TTL);
    assert!(focus_repair_still_viable(1_000, 1_000));
    assert!(focus_repair_still_viable(1_000, 1_000 + ttl));
    assert!(!focus_repair_still_viable(1_000, 1_000 + ttl + 1));
    // A clock that steps backwards leaves the repair viable rather than
    // abandoning it on a negative age.
    assert!(focus_repair_still_viable(1_000, 0));
}

#[test]
fn focus_stranded_uses_evidence_without_snapshot_client_agreement() {
    let (mut snapshot, sidebar, first_work, second_work) = focus_fixture();
    let evidence = snapshot.client_views.clone();
    snapshot.client_views[0].pane_id = first_work;
    let ui = render::ViewState {
        baseline_pane: Some(second_work.clone()),
        ..Default::default()
    };
    for presence in [snapshot.presence, None] {
        snapshot.presence = presence;
        assert_eq!(
            focus_stranded_target(&snapshot, &ui, &sidebar, &evidence, &sidebar, 1_000, 1_050).ok(),
            Some(second_work.clone()),
            "a settled strand must not wait for the pane frame's client sample",
        );
    }
}

#[test]
fn focus_stranded_keeps_empty_evidence_and_tmux_ambiguity_guards() {
    let (mut snapshot, sidebar, _first_work, _second_work) = focus_fixture();
    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ViewState::default(),
            &sidebar,
            &[],
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        None,
    );
    let tmux_sidebar = PaneId::from_parts(MuxName::Tmux, "%10");
    snapshot.client_views[0].client_id = crate::ids::MuxClientId::Tmux("first".to_owned());
    snapshot.client_views[0].pane_id = tmux_sidebar.clone();
    snapshot.client_views.push(crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Tmux("second".to_owned()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%11"),
    });
    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ViewState::default(),
            &tmux_sidebar,
            &[],
            &tmux_sidebar,
            1_000,
            1_050,
        )
        .ok(),
        None,
    );
    snapshot.client_views.pop();
    let mut agreeing_clients = snapshot.client_views.clone();
    agreeing_clients.push(crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Tmux("second".to_owned()),
        pane_id: tmux_sidebar.clone(),
    });
    for views in [snapshot.client_views.clone(), agreeing_clients, Vec::new()] {
        snapshot.client_views = views;
        assert!(
            focus_stranded_target(
                &snapshot,
                &ViewState::default(),
                &tmux_sidebar,
                &[],
                &tmux_sidebar,
                1_000,
                1_050,
            )
            .ok()
            .is_some()
        );
    }
}

#[test]
fn focus_stranded_falls_back_to_working_sibling_when_baseline_is_missing() {
    let (snapshot, sidebar, first_work, _second_work) = focus_fixture();
    let ui = render::ViewState {
        baseline_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_99")),
        ..Default::default()
    };

    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ui,
            &sidebar,
            &snapshot.client_views,
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        Some(first_work),
    );

    let (mut snapshot, sidebar, _first_work, _second_work) = focus_fixture();
    if let Some(view) = &mut snapshot.own_view {
        view.working_pane_ids.clear();
    }

    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ViewState::default(),
            &sidebar,
            &snapshot.client_views,
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        None,
    );
}

#[test]
fn focus_stranded_uses_settled_evidence_despite_stale_viewed_panes() {
    let (mut snapshot, sidebar, first_work, second_work) = focus_fixture();
    snapshot.viewed_panes = vec![sidebar.clone(), first_work];
    let ui = render::ViewState {
        baseline_pane: Some(second_work.clone()),
        ..Default::default()
    };

    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ui,
            &sidebar,
            &snapshot.client_views,
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        Some(second_work.clone()),
    );
    snapshot.client_views.push(crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(2),
        pane_id: sidebar.clone(),
    });
    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ui,
            &sidebar,
            &snapshot.client_views,
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        Some(second_work),
    );
}

#[test]
fn focus_stranded_refuses_distinct_panes_in_settled_evidence() {
    let (mut snapshot, sidebar, first_work, _) = focus_fixture();
    snapshot.client_views.push(crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(2),
        pane_id: first_work.clone(),
    });
    for viewed_panes in [
        vec![],
        vec![sidebar.clone()],
        vec![sidebar.clone(), first_work],
    ] {
        snapshot.viewed_panes = viewed_panes;
        assert_eq!(
            focus_stranded_target(
                &snapshot,
                &ViewState::default(),
                &sidebar,
                &snapshot.client_views,
                &sidebar,
                1_000,
                1_050,
            )
            .ok(),
            None,
        );
    }
}

fn zellij(raw: &str) -> PaneId {
    PaneId::from_parts(crate::MuxName::Zellij, raw)
}

#[test]
fn focus_stranded_ignores_a_fresh_automatic_repair_overlay() {
    let (mut snapshot, sidebar, first_work, _) = focus_fixture();
    snapshot.viewed_panes = vec![sidebar.clone()];
    let evidence = snapshot.client_views.clone();
    let mut anchor = applied_focus_anchor(first_work.clone(), 0, 1_000, None);
    anchor.origin = FocusOrigin::AutomaticRepair;
    anchor.repair_generation = Some(1);
    anchor.pre_action = evidence.clone();
    let (snapshot, _, _) = crate::sidebar::fuse::fuse_owned(
        snapshot,
        crate::sidebar::event_store::EventStore::default(),
        Some(&crate::mux::focus_anchor::FocusPresentation::Target(
            Box::new(anchor),
        )),
        1_050,
    );
    assert!(snapshot.viewed_panes.contains(&sidebar));
    assert!(snapshot.viewed_panes.contains(&first_work));
    assert_eq!(snapshot.client_views, evidence);
    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &ViewState::default(),
            &sidebar,
            &evidence,
            &sidebar,
            1_000,
            1_050,
        )
        .ok(),
        Some(first_work),
    );
}

fn applied_focus_anchor(
    pane_id: PaneId,
    viewport_row: isize,
    stamp_ms: u64,
    order: Option<crate::sidebar_pane::render::FrozenOrder>,
) -> crate::mux::focus_anchor::FocusAnchor {
    crate::mux::focus_anchor::FocusAnchor {
        nonce: crate::ids::FocusNonce::new(),
        session_name: "rimz-test".to_owned(),
        placement: Some(crate::mux::focus_anchor::FocusPlacement {
            row_id: pane_id.to_string(),
            viewport_row,
            held: std::collections::BTreeSet::new(),
        }),
        pane_id,
        origin: crate::mux::focus_anchor::FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: stamp_ms,
        applied_at_ms: Some(stamp_ms),
        state: crate::mux::focus_anchor::FocusIntentState::Applied,
        pre_action: Vec::new(),
        order,
    }
}

fn requested_focus_anchor(
    pane_id: PaneId,
    viewport_row: isize,
    stamp_ms: u64,
    order: Option<crate::sidebar_pane::render::FrozenOrder>,
) -> crate::mux::focus_anchor::FocusAnchor {
    let mut anchor = applied_focus_anchor(pane_id, viewport_row, stamp_ms, order);
    anchor.applied_at_ms = None;
    anchor.state = crate::mux::focus_anchor::FocusIntentState::Requested;
    anchor
}

#[test]
fn a_hidden_fold_retires_its_anchor_in_scratch_after_a_shared_read() {
    let own = zellij("sidebar_0");
    let mut rig = Rig::with_own_pane(own.clone());
    {
        let mut view = rig.view();
        view.ctx().current.own_view = Some(own_view());
        view.ctx().current.viewed_panes.clear();
    }
    rig.views.lock().reconcile(
        &[crate::pane::ClientPaneView {
            client_id: crate::ids::MuxClientId::Zellij(0),
            pane_id: own.clone(),
        }],
        true,
        rig.state.pane.clock.now(),
        Duration::from_secs(60),
    );
    rig.state.frame_timing();
    let shared = rig
        .views
        .lock()
        .presentation(&own, rig.state.pane.clock.now(), false)
        .0;
    rig.views.lock().reconcile(
        &[],
        true,
        rig.state.pane.clock.now(),
        Duration::from_secs(60),
    );
    rig.state.pane.optimistic_watch_until = None;
    let now_ms = rig.state.pane.clock.now_ms();
    let anchor = requested_focus_anchor(zellij("terminal_2"), 0, now_ms - 60_000, None);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    rig.state.fold_transition(|state, v, prepared| {
        assert!(
            state
                .pending_focus_intent(v, prepared, now_ms, state.clock.now())
                .is_none()
        );
    });
    assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());
    assert_eq!(
        rig.state
            .inspect_view(|_, view| view.retired_anchor.map(|(_, stamp)| stamp)),
        Some(anchor.issued_at_ms)
    );
    assert_eq!(
        rig.views.lock().retained(shared).unwrap().retired_anchor,
        None
    );
}

#[test]
fn a_shared_fold_retires_its_anchor_in_that_view() {
    let own = zellij("sidebar_0");
    let mut rig = Rig::with_own_pane(own.clone());
    {
        let mut view = rig.view();
        let (_, context) = view.pair();
        context.current.own_view = Some(SidebarOwnView {
            working_pane_ids: vec![zellij("terminal_2")],
            ..own_view()
        });
        context.current.viewed_panes = vec![zellij("terminal_2")];
    }
    rig.views.lock().reconcile(
        &[crate::pane::ClientPaneView {
            client_id: crate::ids::MuxClientId::Zellij(0),
            pane_id: own,
        }],
        true,
        rig.state.pane.clock.now(),
        Duration::from_secs(60),
    );
    let now_ms = rig.state.pane.clock.now_ms();
    let anchor = requested_focus_anchor(zellij("terminal_2"), 0, now_ms - 60_000, None);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    rig.state.fold_transition(|state, v, prepared| {
        assert!(
            state
                .pending_focus_intent(v, prepared, now_ms, state.clock.now())
                .is_none()
        );
    });
    assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());
    assert_eq!(
        rig.state
            .inspect_view(|_, view| view.retired_anchor.map(|(_, stamp)| stamp)),
        Some(anchor.issued_at_ms)
    );
    rig.view().ctx().current.viewed_panes.clear();
    rig.state.with_view_for_test(Route::READ, |_, _| {});
    rig.views.lock().reconcile(
        &[],
        true,
        rig.state.pane.clock.now(),
        Duration::from_secs(60),
    );
    rig.state.pane.optimistic_watch_until = None;
    rig.state.with_view_for_test(Route::READ, |_, _| {});
    assert_eq!(rig.state.inspect_view(|_, view| view.retired_anchor), None);
}

#[test]
fn retiring_focus_intent_fences_a_sample_taken_after_the_fold_started() {
    let mut rig = Rig::new();
    {
        // The intent targets a pane on this sidebar's viewed tab, which owns it.
        let mut view = rig.view();
        let (_, context) = view.pair();
        context.current.own_view = Some(crate::store::snapshot::SidebarOwnView {
            working_pane_ids: vec![zellij("terminal_2")],
            ..own_view()
        });
        context.current.viewed_panes = vec![zellij("terminal_2")];
    }
    let now_ms = rig.state.pane.clock.now_ms();
    let anchor = requested_focus_anchor(zellij("terminal_2"), 0, now_ms - 60_000, None);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    let stale_fold = Instant::now() - Duration::from_secs(1);
    let sampled_at = Instant::now();
    assert!(
        rig.state
            .fold_transition(|pane, v, prepared| {
                pane.pending_focus_intent(v, prepared, now_ms, stale_fold)
            })
            .is_none()
    );
    assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());
    rig.view().ctx().fold_inputs = Some(Arc::new(super::super::super::fetch::FoldInputs {
        sampled_at,
        anchor: Some(anchor),
        filter: rig.state.inspect_view(|ui, _| ui.make_up_filter.clone()),
    }));
    assert!(
        matches!(
            rig.state.fold_transition(|pane, v, prepared| {
                pane.pending_focus_intent(v, prepared, now_ms, Instant::now())
            }),
            Some(FocusPresentation::Fence)
        ),
        "a pre-retirement sample cannot revive the removed anchor"
    );
}

#[test]
fn focus_push_and_intent_to_a_non_destination_leave_projection_and_nonce() {
    let own = zellij("terminal_99");
    let target = zellij("terminal_2");
    let mut rig = Rig::with_own_pane(own);
    let mut snapshot = snapshot_with_focused_pane(&rig.ws, zellij("terminal_1"));
    snapshot.own_view = Some(own_view());
    snapshot.viewed_panes.clear();
    rig.fold(snapshot, SnapshotSource::Produced);
    let projections = rig.state.pane.projections;
    let selected = rig.view().ui().selected_pane.clone();
    let anchor = requested_focus_anchor(target.clone(), 2, crate::utils::time::unix_now_ms(), None);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    let mut pending = rig.state.views.mirror().clone();
    pending.display_name = "waiting fold".to_owned();
    rig.send(rig.lazy_update(pending, SnapshotSource::Produced));
    let (push, ack) = focus_push(&mut rig, anchor.clone());
    rig.state.on_focus_push(push, &mut rig.terminal).unwrap();
    assert_eq!(
        rig.state.pane.projections, projections,
        "non-owner must leave the publication unread"
    );
    assert_eq!(rig.view().ctx().last_folded_focus_intent, None);
    assert_eq!(rig.view().ui().selected_pane, selected);
    assert_ne!(rig.view().ctx().current.display_name, "waiting fold");
    assert!(!rig.state.watched());
    assert_push_report(&ack, &rig, PushVerdict::NotDestination, false);
    rig.event(SidebarEvent::FocusIntent {
        pane_id: target,
        nonce: anchor.nonce,
    });
    assert_eq!(
        rig.state.pane.projections, projections,
        "a foreign intent leaves the publication unread"
    );
    assert_ne!(rig.view().ctx().current.display_name, "waiting fold");
    assert_eq!(rig.view().ctx().last_folded_focus_intent, None);
}

#[test]
fn focus_push_drains_a_pending_own_view_before_rejecting_the_owner() {
    let (mut pending, sidebar, first, target) = focus_fixture();
    let mut rig = Rig::with_own_pane(sidebar);
    rig.state.pane.prev_width = Some(80);
    pending.focused_pane = Some(first);
    pending.viewed_panes.clear();
    let mut seed = pending.clone();
    seed.own_view = Some(own_view());
    rig.fold(seed, SnapshotSource::Produced);
    let projections = rig.state.pane.projections;
    rig.send(rig.lazy_update(pending, SnapshotSource::Produced));
    rig.state
        .pane
        .paint_hold
        .engage(Instant::now(), crate::utils::time::unix_now_ms());
    let mut anchor =
        requested_focus_anchor(target.clone(), 2, crate::utils::time::unix_now_ms(), None);
    anchor.placement = None;
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    let (push, ack) = focus_push(&mut rig, anchor);
    rig.state.on_focus_push(push, &mut rig.terminal).unwrap();
    assert_eq!(
        ack.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "the pending owner must park, not report not_destination"
    );
    assert!(rig.state.pane.pending_focus_push.is_some());
    assert_eq!(rig.state.pane.projections, projections + 1);
    rig.state.pane.paint_hold.release();
    rig.paint(true);
    assert_eq!(
        rig.view().ui().drawn_selection.as_deref(),
        Some(target.to_string().as_str())
    );
    assert_push_report(&ack, &rig, PushVerdict::Painted, true);
}

#[test]
fn focus_push_with_unknown_own_view_takes_the_drain_and_fold_path() {
    let (mut seed, sidebar, _, target) = focus_fixture();
    let mut rig = Rig::with_own_pane(sidebar);
    seed.own_view = None;
    rig.fold(seed, SnapshotSource::Produced);
    let anchor = requested_focus_anchor(target, 2, crate::utils::time::unix_now_ms(), None);
    let nonce = anchor.nonce;
    let (push, ack) = focus_push(&mut rig, anchor);
    rig.state.on_focus_push(push, &mut rig.terminal).unwrap();
    assert_eq!(rig.view().ctx().last_folded_focus_intent, Some(nonce));
    assert_push_report(&ack, &rig, PushVerdict::NotDestination, true);
}

#[test]
fn focus_push_for_a_target_missing_from_current_still_prepares_and_folds() {
    let mut rig = Rig::with_own_pane(zellij("terminal_99"));
    let mut pending = snapshot_with_focused_pane(&rig.ws, zellij("terminal_1"));
    pending.own_view = Some(own_view());
    pending.display_name = "new target".to_owned();
    assert!(
        rig.view()
            .ctx()
            .current
            .row_owning_pane(&zellij("terminal_2"))
            .is_none()
    );
    rig.send(rig.lazy_update(pending, SnapshotSource::Produced));
    let anchor = requested_focus_anchor(
        zellij("terminal_2"),
        2,
        crate::utils::time::unix_now_ms(),
        None,
    );
    let nonce = anchor.nonce;
    let (push, ack) = focus_push(&mut rig, anchor);
    rig.state.on_focus_push(push, &mut rig.terminal).unwrap();
    assert_eq!(rig.state.pane.projections, 1);
    assert_eq!(rig.view().ctx().current.display_name, "new target");
    assert_eq!(rig.view().ctx().last_folded_focus_intent, Some(nonce));
    assert_push_report(&ack, &rig, PushVerdict::NotDestination, true);
}

#[test]
fn focus_push_and_datagram_fold_each_nonce_once_in_either_order() {
    for push_first in [true, false] {
        let mut rig = Rig::new();
        let first = zellij("terminal_1");
        let target = zellij("terminal_2");
        rig.fold(
            snapshot_with_focused_pane(&rig.ws, first.clone()),
            SnapshotSource::Produced,
        );
        let anchor =
            requested_focus_anchor(target.clone(), 2, crate::utils::time::unix_now_ms(), None);
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        let event = SidebarEvent::FocusIntent {
            pane_id: target.clone(),
            nonce: anchor.nonce,
        };
        let (push, ack) = focus_push(&mut rig, anchor.clone());
        if !push_first {
            rig.event(event.clone());
        }
        rig.state.on_focus_push(push, &mut rig.terminal).unwrap();
        // The target shares this sidebar's tab, so the first delivery folds and the push paints.
        assert_push_report(&ack, &rig, PushVerdict::Painted, push_first);
        assert_eq!(rig.view().ui().selected_pane, Some(target));
        assert_eq!(
            rig.view().ctx().last_folded_focus_intent,
            Some(anchor.nonce)
        );
        let next_frame = Instant::now() + Duration::from_secs(1);
        rig.state.pane.next_frame = next_frame;
        rig.event(event);
        assert_eq!(
            rig.state.pane.next_frame, next_frame,
            "duplicate delivery must not fold again"
        );
    }
}

#[test]
fn only_cross_tab_or_unknown_ownership_jumps_use_the_host_push() {
    let mut rig = Rig::with_own_pane(zellij("terminal_99"));
    let plane = super::super::super::plane::DataPlane::detached();
    rig.state.pane.focus_peers = Some(plane.focus_peers(
        rig.state.pane.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    ));
    rig.view().ctx().current.own_view = Some(own_view());
    assert!(
        rig.state.focus_peers_for(&zellij("terminal_2")).is_some(),
        "cross-tab jumps push"
    );
    assert!(
        rig.state.focus_peers_for(&zellij("terminal_9")).is_none(),
        "same-tab jumps stay synchronous"
    );
    rig.view().ctx().current.own_view = None;
    assert!(
        rig.state.focus_peers_for(&zellij("terminal_9")).is_some(),
        "unknown ownership uses the bounded wait"
    );
}

#[test]
fn focus_push_paints_on_arrival_or_records_the_inline_hold() {
    for held in [false, true] {
        let (mut snapshot, sidebar, first, target) = focus_fixture();
        let mut rig = Rig::with_own_pane(sidebar);
        snapshot.focused_pane = Some(first);
        snapshot.viewed_panes.clear();
        rig.fold(snapshot, SnapshotSource::Produced);
        if held {
            rig.state
                .pane
                .paint_hold
                .engage(Instant::now(), crate::utils::time::unix_now_ms());
        }
        let mut anchor = requested_focus_anchor(target, 2, crate::utils::time::unix_now_ms(), None);
        anchor.placement = None;
        let (push, ack) = focus_push(&mut rig, anchor);
        rig.state.on_focus_push(push, &mut rig.terminal).unwrap();
        if held {
            assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
            assert_eq!(
                rig.state
                    .pane
                    .pending_focus_push
                    .as_ref()
                    .unwrap()
                    .peer
                    .blocked_by,
                Some(PushBlock::PaintHold),
                "the handler must run the existing paint gates"
            );
            rig.state.pane.pending_focus_push.as_mut().unwrap().until = Instant::now();
            rig.paint(true);
            assert_push_report(&ack, &rig, PushVerdict::Expired, true);
        } else {
            assert_push_report(&ack, &rig, PushVerdict::Painted, true);
            assert!(rig.state.pane.pending_focus_push.is_none());
        }
    }
}

fn assert_foreign_focus_anchor_is_ignored(shared: bool) {
    let mut rig = Rig::new();
    let first = zellij("terminal_1");
    let target = zellij("terminal_2");
    rig.fold(
        snapshot_with_focused_pane(&rig.ws, first.clone()),
        SnapshotSource::Produced,
    );
    let mut anchor =
        requested_focus_anchor(target.clone(), 7, crate::utils::time::unix_now_ms(), None);
    anchor.session_name = "another-session".to_owned();
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).expect("store foreign anchor");

    for focused in [first, target] {
        let snapshot = snapshot_with_focused_pane(&rig.ws, focused.clone());
        if shared {
            rig.shared_fold(
                snapshot,
                Arc::new(super::super::super::fetch::FoldInputs {
                    anchor: Some(anchor.clone()),
                    ..Default::default()
                }),
            );
        } else {
            rig.fold(snapshot, SnapshotSource::Produced);
        }
        assert_eq!(rig.view().ui().selected_pane, Some(focused));
        assert!(rig.view().ui().focus_placement.is_none());
        assert_eq!(rig.view().ui().last_focus_anchor_ms, 0);
        assert_eq!(rig.view().ctx().confirmed_focus_intent_ms, 0);
        assert_eq!(
            crate::mux::focus_anchor::load(&rig.runtime)
                .expect("a foreign fold must not retire the anchor")
                .nonce,
            anchor.nonce,
        );
    }
}

#[test]
fn live_foreign_focus_anchor_is_neither_presented_nor_retired() {
    assert_foreign_focus_anchor_is_ignored(false);
}

#[test]
fn shared_foreign_focus_anchor_is_neither_presented_nor_retired() {
    assert_foreign_focus_anchor_is_ignored(true);
}

#[test]
fn older_shared_inputs_do_not_undo_a_consumed_focus_intent() {
    let mut rig = Rig::new();
    let first = zellij("terminal_1");
    let target = zellij("terminal_2");
    let snapshot = snapshot_with_focused_pane(&rig.ws, first);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let inputs = Arc::new(super::super::super::fetch::FoldInputs::default());
    let anchor = requested_focus_anchor(target.clone(), 7, crate::utils::time::unix_now_ms(), None);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    rig.event(SidebarEvent::FocusIntent {
        pane_id: target.clone(),
        nonce: anchor.nonce,
    });
    assert_eq!(rig.view().ui().selected_pane, Some(target.clone()));
    assert_eq!(rig.view().ui().scroll_offset, 0);

    rig.deliver(FetchUpdate::Shared {
        update: Box::new(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        }),
        context: Arc::new(super::super::super::fetch::FoldShared {
            inputs,
            ..Default::default()
        }),
    });
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(target),
        "an older shared cut must not rewind an already consumed focus intent"
    );
    assert_eq!(rig.view().ui().scroll_offset, 0);
    assert_eq!(
        crate::mux::focus_anchor::load(&rig.runtime).unwrap().nonce,
        anchor.nonce
    );
}

#[test]
fn shared_anchor_keeps_selection_and_fresh_nonce_pane_local() {
    let mut rig = Rig::new();
    let stamp = crate::utils::time::unix_now_ms();
    let old = applied_focus_anchor(zellij("terminal_1"), 7, stamp, None);
    let new = requested_focus_anchor(zellij("terminal_2"), 9, stamp + 1, None);
    rig.view().ctx().current = snapshot_with_focused_pane(&rig.ws, old.pane_id.clone());
    crate::mux::focus_anchor::store(&rig.runtime, &new).unwrap();
    rig.view().ctx().fold_inputs = Some(Arc::new(super::super::super::fetch::FoldInputs {
        anchor: Some(old.clone()),
        ..Default::default()
    }));
    rig.view().ui().selected_pane = Some(old.pane_id.clone());
    rig.view().ui().scroll_offset = 2;
    rig.state.fold_transition(PaneState::apply_focus_anchor);
    assert_eq!(rig.view().ui().focus_placement, old.placement);
    assert_eq!(
        rig.view().ui().scroll_offset,
        2,
        "apply the fold's anchor against this pane's selection"
    );

    rig.view().ui().selected_pane = Some(new.pane_id.clone());
    rig.view().ui().last_focus_anchor_ms = 0;
    rig.view().ui().scroll_offset = 2;
    rig.state.fold_transition(PaneState::apply_focus_anchor);
    assert_eq!(
        rig.view().ui().scroll_offset,
        2,
        "a different selection must not adopt the cached anchor"
    );
    assert_eq!(rig.view().ui().last_focus_anchor_ms, 0);

    rig.view().ui().selected_pane = Some(old.pane_id.clone());
    rig.view().ctx().confirmed_focus_intent_ms = old.issued_at_ms;
    rig.state.fold_transition(PaneState::apply_focus_anchor);
    assert_eq!(
        crate::mux::focus_anchor::load(&rig.runtime).unwrap().nonce,
        new.nonce,
        "live nonce comparison must preserve an intent written after the shared read"
    );
    assert_eq!(rig.view().ctx().confirmed_focus_intent_ms, 0);

    rig.view().ctx().fold_inputs = None;
    rig.view().ui().selected_pane = Some(new.pane_id.clone());
    rig.state.fold_transition(PaneState::apply_focus_anchor);
    assert_eq!(rig.view().ui().focus_placement, new.placement);
    assert_eq!(
        rig.view().ui().scroll_offset,
        2,
        "the event path reads the newer live anchor"
    );
}

#[test]
fn shared_focus_cut_converges_missed_intents_and_confirmation() {
    let mut rig = Rig::new();
    let first = zellij("terminal_1");
    let target = zellij("terminal_2");
    let snapshot = snapshot_with_focused_pane(&rig.ws, first.clone());
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let stamp = crate::utils::time::unix_now_ms();
    let mut anchor = requested_focus_anchor(target.clone(), 7, stamp, None);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    rig.shared_fold(
        snapshot.clone(),
        Arc::new(super::super::super::fetch::FoldInputs {
            anchor: Some(anchor.clone()),
            ..Default::default()
        }),
    );
    assert_eq!(rig.view().ui().selected_pane, Some(target.clone()));
    assert_eq!(rig.view().ui().scroll_offset, 0);

    anchor.state = crate::mux::focus_anchor::FocusIntentState::Applied;
    anchor.applied_at_ms = Some(stamp);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    let applied_inputs = Arc::new(super::super::super::fetch::FoldInputs {
        anchor: Some(anchor),
        ..Default::default()
    });
    let mut confirmed = snapshot.clone();
    confirmed.panes_observed_at_ms = Some(stamp);
    confirmed.focused_pane = Some(target.clone());
    confirmed.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(1),
        pane_id: target.clone(),
    }];
    rig.shared_fold(confirmed, applied_inputs.clone());
    assert_eq!(rig.view().ui().selected_pane, Some(target.clone()));
    assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());

    rig.view().ui().scroll_offset = 3;
    rig.shared_fold(snapshot.clone(), applied_inputs);
    assert_eq!(rig.view().ui().selected_pane, Some(target));
    assert_eq!(rig.view().ui().scroll_offset, 3);
    rig.shared_fold(snapshot, Arc::default());
    assert_eq!(rig.view().ui().selected_pane, Some(first));
}

#[test]
fn older_shared_inputs_do_not_renew_an_observed_receipt() {
    let mut rig = Rig::new();
    rig.state.pane.diag = crate::diag::DiagSink::under(
        rig._dir.path().to_path_buf(),
        rig.ws.clone(),
        "rimz-test",
        None,
    );
    let trace = crate::StatePaths::class_path(
        rig._dir.path(),
        crate::disk::paths::Class::Audit,
        "notify.log.jsonl",
    );
    let mut snapshot = agent_snapshot(&rig.ws);
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    row.last_activity = jiff::Timestamp::UNIX_EPOCH;
    if let Some(agent) = row.as_agent_mut() {
        agent.status = crate::agents::AgentStatus::Waiting;
    }
    let pane = row.pane.as_ref().unwrap().pane_id.clone();
    snapshot.focused_pane = Some(pane.clone());
    snapshot.viewed_panes = vec![pane];
    let inputs = Arc::default();
    crate::sidebar::read_marks::write_read_marks(&rig.runtime, ["agent-1".to_owned()], 1_000)
        .unwrap();
    rig.shared_fold(snapshot.clone(), inputs);
    assert!(
        !trace.exists(),
        "a stale cut must not emit another clear trace"
    );
    assert_eq!(
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime).cleared_at_ms("agent-1"),
        Some(1_000),
        "a stale shared receipt baseline must not renew a pane's existing clear"
    );
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .last_activity = jiff::Timestamp::now();
    rig.shared_fold(snapshot.clone(), Arc::default());
    let renewed =
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime).cleared_at_ms("agent-1");
    assert!(
        renewed > Some(1_000),
        "a new needs-a-look episode still receives a new receipt"
    );
    std::thread::sleep(Duration::from_millis(2));
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .last_activity = jiff::Timestamp::now();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = true;
    rig.shared_fold(snapshot, Arc::default());
    assert!(
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime).cleared_at_ms("agent-1")
            > renewed,
        "a new unread episode still receives a new receipt"
    );
    assert!(
        trace.exists(),
        "the later attention episode emits its clear trace"
    );
}

#[test]
fn older_shared_inputs_preserve_a_consumed_native_focus_event() {
    let mut rig = Rig::new();
    let first = zellij("terminal_1");
    let target = zellij("terminal_2");
    let mut snapshot = snapshot_with_focused_pane(&rig.ws, first.clone());
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let inputs = Arc::default();
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![target.clone()],
        unfocused: vec![first],
    });
    assert_eq!(rig.view().ctx().current.focused_pane, Some(target.clone()));
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .name = "updated by pull".to_owned();
    rig.shared_fold(snapshot, inputs);
    assert!(
        rig.view()
            .ctx()
            .current
            .rows()
            .any(|row| row.name == "updated by pull"),
        "an older input cut still applies the complete pulled snapshot"
    );
    assert_eq!(
        rig.view().ctx().current.focused_pane,
        Some(target.clone()),
        "a live absence is not an intent-retirement fence over native focus"
    );
    assert_eq!(rig.view().ui().selected_pane, Some(target));
}

#[test]
fn rowless_focus_anchor_preserves_scroll_on_matching_fold() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");
    let stamp_ms = crate::utils::time::unix_now_ms();
    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    let order = layout_freeze::capture_order(&snapshot, rig.view().ui());
    let mut anchor = applied_focus_anchor(target.clone(), 7, stamp_ms, Some(order.clone()));
    anchor.placement = None;
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).expect("store anchor");
    rig.view().ui().scroll_offset = 2;

    rig.fold(snapshot, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().selected_pane, Some(target));
    assert_eq!(rig.view().ui().scroll_offset, 2);
    assert_eq!(rig.view().ui().last_focus_anchor_ms, stamp_ms);
    assert_eq!(rig.view().ui().freeze.as_ref().unwrap().frozen, order);
}

#[test]
fn fresh_requested_focus_anchor_installs_shared_hold_once() {
    let mut rig = Rig::new();
    let first = zellij("terminal_1");
    let target = zellij("terminal_2");
    let stamp_ms = crate::utils::time::unix_now_ms() - 1_000;
    let mut anchor = requested_focus_anchor(
        target.clone(),
        7,
        stamp_ms,
        Some(crate::sidebar_pane::render::FrozenOrder {
            groups: vec!["/repo/main".to_owned()],
            rows: vec![
                crate::sidebar_pane::render::FrozenRow {
                    id: target.to_string(),
                    pane: Some(target.to_string()),
                },
                crate::sidebar_pane::render::FrozenRow {
                    id: first.to_string(),
                    pane: Some(first.to_string()),
                },
            ],
            visible: HashSet::from([target.to_string()]),
        }),
    );
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).expect("store anchor");

    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    let before = Timestamp::now().as_millisecond();
    rig.fold(snapshot, SnapshotSource::Produced);
    let after = Timestamp::now().as_millisecond();

    assert_eq!(rig.view().ui().selected_pane, Some(target.clone()));
    assert_eq!(rig.view().ui().scroll_offset, 0);
    assert_eq!(rig.view().ui().focus_placement, anchor.placement);
    assert_eq!(
        rig.view().ctx().current.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.pane.as_ref().expect("pane").pane_id.clone())
            .collect::<Vec<_>>(),
        vec![target.clone(), first],
        "fold snapshot adopts anchor row order before paint"
    );
    let hold = rig.view().ui().freeze.clone().expect("shared hold");
    assert_eq!(hold.frozen.visible, HashSet::from([target.to_string()]));
    assert!(
        (before..=after).contains(&hold.armed_ms),
        "adoption starts at the fold, not the anchor stamp"
    );

    rig.view().ui().scroll_offset = 4;
    rig.view().ui().focus_placement = None;
    anchor.state = crate::mux::focus_anchor::FocusIntentState::Applied;
    anchor.applied_at_ms = Some(crate::utils::time::unix_now_ms());
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).expect("apply anchor");
    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().scroll_offset, 4);
    assert!(rig.view().ui().focus_placement.is_none());
    assert_eq!(rig.view().ui().last_focus_anchor_ms, stamp_ms);
}

#[test]
fn fresh_child_focus_anchor_selects_parent_and_queues_placement_and_hold() {
    let mut rig = Rig::new();
    let parent = zellij("terminal_2");
    let first = zellij("terminal_1");
    let child = zellij("terminal_child");
    let stamp_ms = crate::utils::time::unix_now_ms();
    let order = crate::sidebar_pane::render::FrozenOrder {
        groups: vec!["/repo/main".to_owned()],
        rows: vec![
            crate::sidebar_pane::render::FrozenRow {
                id: parent.to_string(),
                pane: Some(parent.to_string()),
            },
            crate::sidebar_pane::render::FrozenRow {
                id: first.to_string(),
                pane: Some(first.to_string()),
            },
        ],
        visible: HashSet::from([parent.to_string()]),
    };
    let mut anchor = requested_focus_anchor(child.clone(), 7, stamp_ms, Some(order.clone()));
    anchor.placement.as_mut().unwrap().row_id = parent.to_string();
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    let mut snapshot = snapshot_with_focused_pane(&rig.ws, parent.clone());
    let row = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )
    .iter_mut()
    .find(|row| row.pane.as_ref().unwrap().pane_id == parent)
    .unwrap();
    row.card = crate::store::snapshot::RowCard::Agent(std::sync::Arc::default());
    row.as_agent_mut().unwrap().sub_agents.push(
        serde_json::from_value(serde_json::json!({
            "id": "child", "kind": "claude", "name": "explorer", "status": "running",
            "last_activity": snapshot.now, "pane": child,
        }))
        .unwrap(),
    );
    // The child's pane opened on this sidebar's tab, so its intent is this sidebar's.
    snapshot
        .own_view
        .as_mut()
        .unwrap()
        .working_pane_ids
        .push(child.clone());
    snapshot.focused_pane = Some(child);
    rig.fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().selected_pane, Some(parent));
    assert_eq!(rig.view().ui().scroll_offset, 0);
    assert_eq!(rig.view().ui().focus_placement, anchor.placement);
    assert_eq!(rig.view().ui().freeze.as_ref().unwrap().frozen, order);
    assert_eq!(rig.view().ui().last_focus_anchor_ms, stamp_ms);
}

#[test]
fn focus_anchor_stamp_applies_once() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");
    let stamp_ms = crate::utils::time::unix_now_ms();
    crate::mux::focus_anchor::store(
        &rig.runtime,
        &applied_focus_anchor(target.clone(), 7, stamp_ms, None),
    )
    .expect("store anchor");

    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    rig.fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().scroll_offset, 0);
    assert!(rig.view().ui().focus_placement.is_some());

    rig.view().ui().scroll_offset = 4;
    rig.view().ui().focus_placement = None;
    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().scroll_offset, 4);
    assert!(rig.view().ui().focus_placement.is_none());
}

#[test]
fn stale_focus_anchor_fences_unchanged_observation_to_unknown() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");
    let ttl_ms = crate::mux::focus_anchor::FOCUS_ANCHOR_FRESH.as_millis() as u64;
    let stale_stamp = crate::utils::time::unix_now_ms().saturating_sub(ttl_ms + 1);
    crate::mux::focus_anchor::store(
        &rig.runtime,
        &applied_focus_anchor(target.clone(), 7, stale_stamp, None),
    )
    .expect("store anchor");
    rig.view().ui().scroll_offset = 3;

    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().selected_pane, None);
    assert_eq!(rig.view().ui().scroll_offset, 3);
    assert_eq!(rig.view().ui().last_focus_anchor_ms, 0);
}

#[test]
fn superseding_client_observation_leaves_scroll_untouched() {
    let mut rig = Rig::new();
    let selected = zellij("terminal_2");
    let stamp_ms = crate::utils::time::unix_now_ms();
    let mut anchor = applied_focus_anchor(zellij("terminal_1"), 7, stamp_ms, None);
    anchor.pre_action = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(7),
        pane_id: anchor.pane_id.clone(),
    }];
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).expect("store anchor");
    rig.view().ui().scroll_offset = 3;

    let mut snapshot = snapshot_with_focused_pane(&rig.ws, selected.clone());
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::Active);
    snapshot.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(7),
        pane_id: selected.clone(),
    }];
    rig.fold(snapshot, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().selected_pane, Some(selected));
    assert_eq!(rig.view().ui().scroll_offset, 3);
    assert_eq!(rig.view().ui().last_focus_anchor_ms, 0);
}

#[test]
fn reload_key_preserves_control_reload_flow_without_freezing_order() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    let wakeup = Wakeup::Press {
        code: KeyCode::Char('r'),
        mods: KeyModifiers::NONE,
    };
    let mut rig = Rig::new();
    let (requests, received) = std::sync::mpsc::channel();
    rig.state.pane.request_reload = Box::new(move || {
        requests.send(()).unwrap();
    });
    rig.state.on_wakeup(&mut rig.terminal, wakeup).unwrap();
    assert_eq!(received.try_recv(), Ok(()));
    assert!(received.try_recv().is_err());
    assert!(
        rig.view().ui().freeze.is_none(),
        "reload must not freeze the refreshed order"
    );
}

#[test]
fn input_browse_arms_freeze_before_next_fold() {
    let mut rig = Rig::new();
    let panes = vec![
        pane("terminal_1", "tab_0", false),
        pane("terminal_2", "tab_0", false),
    ];
    let first = panes[0].pane_id.clone();
    let second = panes[1].pane_id.clone();
    rig.view().ctx().current = snapshot_with_panes(&rig.ws, panes);
    rig.view().ui().selected_pane = Some(first);
    rig.view().ui().selected_index = Some(0);
    {
        let mut guard = rig.view();
        let (ui, context) = guard.pair();
        ui.last_order = super::super::layout_freeze::capture_order(&context.current, ui);
    }

    rig.state
        .apply_input([Wakeup::Key(KeyAction::Down)], &mut rig.terminal)
        .unwrap();

    assert_eq!(rig.view().ui().selected_pane, Some(second));
    assert!(
        rig.view().ui().freeze.is_some(),
        "arrow-key browse arms the layout freeze immediately, without waiting for a fold"
    );
}

#[test]
fn browse_survives_a_frameless_escape_hatch_with_a_live_freeze() {
    let mut rig = Rig::new();
    let framed = agent_snapshot(&rig.ws);
    let selected = framed.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    rig.fold(framed.clone(), SnapshotSource::Produced);
    rig.input(KeyAction::Down);
    let browse = rig.view().ui().browse.clone();
    assert!(browse.is_some());
    assert!(rig.view().ui().freeze.is_some());
    assert_eq!(rig.view().ui().selected_pane, Some(selected.clone()));

    let mut frameless = framed.clone();
    frameless.panes_produced_at_ms = None;
    std::sync::Arc::make_mut(&mut frameless.worktree_groups).clear();
    rig.fold(frameless.clone(), SnapshotSource::Cached);
    assert_eq!(rig.view().ctx().gate.reject_streak, 1);
    assert_eq!(rig.view().ctx().current.panes_produced_at_ms, Some(1));
    assert_eq!(rig.view().ui().selected_pane, Some(selected.clone()));
    assert_eq!(rig.view().ui().browse, browse);

    let now_ms = jiff::Timestamp::now().as_millisecond();
    rig.view().ctx().gate.rejecting_since =
        Some(jiff::Timestamp::from_millisecond(now_ms - 1_000).unwrap());
    rig.fold(frameless, SnapshotSource::Cached);

    assert_eq!(rig.view().ctx().gate.reject_streak, 0);
    assert_eq!(rig.view().ctx().current.panes_produced_at_ms, None);
    assert!(rig.view().ctx().current.worktree_groups.is_empty());
    assert!(rig.view().ui().freeze.is_some());
    assert_eq!(rig.view().ui().selected_pane, Some(selected.clone()));
    assert_eq!(rig.view().ui().browse, browse);
    assert_eq!(rig.view().ui().selected_index, None);

    rig.fold(framed, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().selected_pane, Some(selected));
    assert_eq!(rig.view().ui().browse, browse);
    assert_eq!(rig.view().ui().selected_index, Some(0));
}

#[test]
fn answering_focused_agent_holds_the_pre_answer_order() {
    let mut rig = Rig::new();
    let mut before = agent_snapshot(&rig.ws);
    let selected = before.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .expect("agent pane")
        .pane_id
        .clone();
    let mut other = before.worktree_groups[0].rows[0].clone();
    other.id = "agent-2".to_owned();
    other.pane = Some(pane("terminal_1", "tab_0", false));
    other.attention_score = 200;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut before.worktree_groups)[0].rows)
        [0]
    .attention_score = 600;
    set_agent_status(&mut before, crate::agents::AgentStatus::Waiting);
    other.as_agent_mut().expect("agent row").status = crate::agents::AgentStatus::Running;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut before.worktree_groups)[0].rows)
        .push(other);
    before.focused_pane = Some(selected.clone());
    // Only the sidebar itself is viewed: the sidebar watches, and the fold stays independent of
    // the existing focus-read hold trigger.
    before.viewed_panes = vec![rig.state.pane.config.own_pane.clone()];
    before.sort_groups_for_presentation();
    rig.view().ctx().current = before.clone();
    rig.view().ui().selected_pane = Some(selected.clone());
    rig.view().ui().baseline_pane = Some(selected);
    {
        let mut guard = rig.view();
        let (ui, context) = guard.pair();
        ui.last_order = super::super::layout_freeze::capture_order(&context.current, ui);
    }

    let mut after = before;
    after.panes_produced_at_ms = Some(2);
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut after.worktree_groups)[0].rows)
        [0]
    .attention_score = 200;
    set_agent_status(&mut after, crate::agents::AgentStatus::Running);
    let mut ranked = after.clone();
    ranked.sort_groups_for_presentation();
    assert_eq!(
        ranked.worktree_groups[0].rows[0].id, "agent-2",
        "the live rank moves the answered row down"
    );

    rig.fold(after, SnapshotSource::Produced);

    assert!(rig.view().ui().freeze.is_some());
    assert_eq!(
        rig.view().ctx().current.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["agent-1", "agent-2"]
    );
}

#[test]
fn mark_all_read_clears_every_unread_row_and_writes_receipts() {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    let mut second = snapshot.worktree_groups[0].rows[0].clone();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .id = "agent-1".to_owned();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = true;
    second.id = "agent-2".to_owned();
    second.unread = true;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(second);
    rig.view().ctx().current = snapshot;
    rig.view().ui().unread_guard = Some("agent-1".to_owned());
    {
        let mut guard = rig.view();
        let (ui, context) = guard.pair();
        ui.last_order = super::super::layout_freeze::capture_order(&context.current, ui);
    }

    rig.input(KeyAction::MarkAllRead);

    assert!(
        rig.view()
            .ctx()
            .current
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .all(|row| !row.unread),
        "all unread bits clear locally"
    );
    assert_eq!(rig.view().ui().unread_guard, None);
    let marks = crate::sidebar::read_marks::ReadMarks::load(&rig.runtime);
    assert!(marks.cleared_at_ms("agent-1").is_some());
    assert!(marks.cleared_at_ms("agent-2").is_some());
    assert!(rig.state.pane.dirty);
    assert!(
        rig.next_request().is_some(),
        "mark-all schedules a convergence refetch"
    );
}
