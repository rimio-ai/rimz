use super::*;
use crate::ids::MuxClientId;
use crate::pane::ClientPaneView;
use crate::sidebar_pane::app::card_commands::CardInput;
use crate::sidebar_pane::{CardDirection, CardReply, CardRequest};

fn command_rig() -> (Rig, SidebarSnapshot) {
    let clock = LoopClock::play("2026-11-07T12:00:00Z".parse().unwrap());
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let mut rig = Rig::with_clock(Some(own.clone()), BodyLens::default(), None, clock);
    let mut snapshot = snapshot_with_panes(
        &rig.ws,
        (1..=4)
            .map(|n| pane(&format!("terminal_{n}"), "tab_0", false))
            .collect(),
    );
    snapshot.now = rig.state.pane.clock.timestamp();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: 4,
        working_pane_ids: snapshot
            .rows()
            .flat_map(|row| row.pane_ids())
            .cloned()
            .collect(),
        own_view_is_daemon: false,
    });
    snapshot.viewed_panes = vec![own.clone()];
    snapshot.client_views = vec![ClientPaneView {
        client_id: MuxClientId::Zellij(1),
        pane_id: own,
    }];
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let now = rig.state.pane.clock.now();
    rig.views
        .lock()
        .reconcile(&snapshot.client_views, true, now, Duration::from_secs(60));
    (rig, snapshot)
}

fn queued(rig: &Rig, request: CardRequest) -> (Wakeup, Receiver<CardReply>) {
    let now = rig.state.pane.clock.now();
    let (view, _) = rig
        .views
        .lock()
        .presentation(&rig.state.pane.config.own_pane, now, false);
    let (reply, receive) = std::sync::mpsc::sync_channel(1);
    (
        Wakeup::Card(Arc::new(CardInput {
            answer: std::sync::Mutex::new(None),
            request,
            reply,
            expires: now + Duration::from_secs(5),
            view,
        })),
        receive,
    )
}

fn command(rig: &mut Rig, request: CardRequest) -> CardReply {
    let (input, reply) = queued(rig, request);
    rig.state.on_input(input, &mut rig.terminal).unwrap();
    let reply = reply.try_recv();
    assert!(reply.is_ok(), "reply follows effect dispatch: {reply:?}");
    reply.unwrap()
}

fn sample_focus(rig: &mut Rig, snapshot: &mut SidebarSnapshot, focused: PaneId) {
    snapshot.focused_pane = Some(focused.clone());
    snapshot.viewed_panes = vec![focused.clone()];
    snapshot.client_views[0].pane_id = focused;
    let mut panes: Vec<_> = snapshot.rows().filter_map(|row| row.pane.clone()).collect();
    let mut own = pane("terminal_99", "tab_0", false);
    own.command = Some("rimz-sidebar".into());
    panes.push(own);
    let mut frame =
        crate::sidebar::frame::assemble_frame(panes, rig.state.pane.clock.now_ms(), "rimz-test");
    frame.client_views.clone_from(&snapshot.client_views);
    frame.client_view_fresh = true;
    rig.deliver(FetchUpdate::Fold {
        fold: Arc::new(WorkspaceFold {
            sampled_at: rig.state.pane.clock.now(),
            workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot.clone()),
            frame: Some(Arc::new(frame)),
        }),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });
}

#[test]
fn card_last_returns_before_an_earlier_jump_lands_in_either_worker_order() {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusOrigin};

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for earlier_lands_first in [true, false] {
        let (mut rig, mut snapshot) = command_rig();
        let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
        sample_focus(&mut rig, &mut snapshot, pane(2));
        sample_focus(&mut rig, &mut snapshot, pane(3));
        let peers = rig
            .focus_plane
            .focus_peers(
                rig.state.pane.config.instance_id.clone(),
                crate::diag::DiagSink::disabled(),
            )
            .without_push();
        assert_eq!(
            command(&mut rig, CardRequest::Slot { n: 2 }),
            CardReply::Done
        );
        let earlier = peers.arm(pane(2), rig.state.pane.clock.now());
        rig.state.pane.clock.advance(Duration::from_millis(30));
        assert_eq!(command(&mut rig, CardRequest::Last), CardReply::Done);
        let returned = rig.view().ui().selected_pane.clone();
        let target = returned.clone().unwrap();
        let latest = peers.arm(target.clone(), rig.state.pane.clock.now());
        let mut anchor = FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "rimz-test".into(),
            pane_id: pane(2),
            origin: FocusOrigin::User,
            repair_generation: None,
            input_at_ms: None,
            issued_at_ms: rig.state.pane.clock.now_ms(),
            applied_at_ms: None,
            state: FocusIntentState::Requested,
            pre_action: snapshot.client_views.clone(),
            placement: None,
            order: None,
        };
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        rig.state.fold_focus_intent(anchor.clone());
        let mut native = Vec::new();
        if earlier_lands_first {
            // The first mux action was already in flight when the return input arrived.
            anchor.state = FocusIntentState::Applied;
            anchor.applied_at_ms = Some(rig.state.pane.clock.now_ms());
            crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
            rig.state.fold_focus_intent(anchor.clone());
            native.push(pane(2));
            sample_focus(&mut rig, &mut snapshot, pane(2));
        }
        let held = rig.view().ui().selected_pane.clone();
        rig.state.pane.clock.advance(Duration::from_millis(1));
        anchor.nonce = crate::ids::FocusNonce::new();
        anchor.pane_id = target.clone();
        anchor.state = FocusIntentState::Applied;
        anchor.issued_at_ms = rig.state.pane.clock.now_ms();
        anchor.applied_at_ms = Some(anchor.issued_at_ms);
        assert!(
            latest
                .request(|| {
                    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
                    native.push(target.clone());
                })
                .is_some()
        );
        rig.state.fold_focus_intent(anchor);
        sample_focus(&mut rig, &mut snapshot, target.clone());
        assert!(
            earlier
                .request(|| panic!("an older worker must not dispatch after the return"))
                .is_none()
        );
        rig.state.pane.clock.advance(Duration::from_secs(6));
        sample_focus(&mut rig, &mut snapshot, target);
        let settled = rig.view().ui().selected_pane.clone();
        assert_eq!(command(&mut rig, CardRequest::Last), CardReply::Done);
        let toggled = rig.view().ui().selected_pane.clone();
        actual.push((returned, held, settled, toggled, native));
        let native = if earlier_lands_first {
            vec![pane(2), pane(3)]
        } else {
            vec![pane(3)]
        };
        expected.push((
            Some(pane(3)),
            Some(pane(3)),
            Some(pane(3)),
            Some(pane(2)),
            native,
        ));
    }
    assert_eq!(
        actual, expected,
        "return target, held pick, settled pick, toggle, native dispatches"
    );
}

#[test]
fn card_walk_repeats_while_pending_and_uses_the_frozen_order_after_a_fold() {
    let (mut rig, mut snapshot) = command_rig();
    for n in 1..=3 {
        assert_eq!(
            command(
                &mut rig,
                CardRequest::Walk {
                    direction: CardDirection::Next,
                    worktree: false,
                }
            ),
            CardReply::Done
        );
        let target = PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
        assert_eq!(rig.view().ui().selected_pane, Some(target.clone()));
        assert_eq!(rig.view().ctx().pending_input_focus, Some(target));
        assert_eq!(rig.view().ui().baseline_pane, None);
    }
    assert!(rig.view().ui().freeze.is_some());
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).reverse();
    rig.fold(snapshot, SnapshotSource::Produced);
    assert_eq!(
        command(
            &mut rig,
            CardRequest::Walk {
                direction: CardDirection::Next,
                worktree: false,
            }
        ),
        CardReply::Done
    );
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_4"))
    );
}

#[test]
fn card_walk_keeps_the_latest_pick_when_an_earlier_jump_lands() {
    let (mut rig, mut snapshot) = command_rig();
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(rig.view().ui().baseline_pane, Some(pane(4)));
    let mut inputs = Vec::new();
    for n in 1..=2 {
        assert_eq!(
            command(
                &mut rig,
                CardRequest::Walk {
                    direction: CardDirection::Next,
                    worktree: false,
                }
            ),
            CardReply::Done
        );
        assert_eq!(rig.view().ui().selected_pane, Some(pane(n)));
        inputs.push(rig.view().ctx().focus_rollback.as_ref().unwrap().input);
    }
    rig.state
        .on_wakeup(
            &mut rig.terminal,
            Wakeup::FocusDispatched {
                input: inputs[0],
                nonce: crate::ids::FocusNonce::new(),
            },
        )
        .unwrap();
    sample_focus(&mut rig, &mut snapshot, pane(1));
    assert_eq!(rig.view().ui().baseline_pane, Some(pane(1)));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(2)));
    assert_eq!(rig.view().ctx().pending_input_focus, Some(pane(2)));
    assert_eq!(
        command(
            &mut rig,
            CardRequest::Walk {
                direction: CardDirection::Next,
                worktree: false,
            }
        ),
        CardReply::Done
    );
    assert_eq!(rig.view().ui().selected_pane, Some(pane(3)));
}

#[test]
fn card_walk_ack_before_a_new_sample_keeps_the_pick_for_the_next_walk() {
    let (mut rig, mut snapshot) = command_rig();
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    let walk = CardRequest::Walk {
        direction: CardDirection::Next,
        worktree: false,
    };
    assert_eq!(command(&mut rig, walk.clone()), CardReply::Done);
    let input = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
    rig.state
        .on_wakeup(
            &mut rig.terminal,
            Wakeup::FocusDispatched {
                input,
                nonce: crate::ids::FocusNonce::new(),
            },
        )
        .unwrap();
    assert_eq!(rig.view().ctx().pending_input_focus, Some(pane(1)));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(1)));
    assert_eq!(command(&mut rig, walk), CardReply::Done);
    assert_eq!(rig.view().ui().selected_pane, Some(pane(2)));
}

#[test]
fn card_pick_ack_ignores_an_earlier_anchor_with_the_same_timestamp() {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusOrigin};

    let (mut rig, mut snapshot) = command_rig();
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    let earlier = FocusAnchor {
        nonce: crate::ids::FocusNonce::new(),
        session_name: "rimz-test".into(),
        pane_id: pane(1),
        origin: FocusOrigin::User,
        repair_generation: None,
        input_at_ms: None,
        issued_at_ms: rig.state.pane.clock.now_ms(),
        applied_at_ms: Some(rig.state.pane.clock.now_ms()),
        state: FocusIntentState::Applied,
        pre_action: snapshot.client_views.clone(),
        placement: None,
        order: None,
    };
    assert_eq!(
        command(&mut rig, CardRequest::Slot { n: 2 }),
        CardReply::Done
    );
    let input = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
    rig.state
        .on_wakeup(
            &mut rig.terminal,
            Wakeup::FocusDispatched {
                input,
                nonce: crate::ids::FocusNonce::new(),
            },
        )
        .unwrap();
    crate::mux::focus_anchor::store(&rig.runtime, &earlier).unwrap();
    rig.state.fold_focus_intent(earlier);
    assert_eq!(rig.view().ctx().pending_input_focus, Some(pane(2)));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(2)));
}

fn delayed_earlier_walk(applied: bool) -> Vec<[Option<PaneId>; 3]> {
    use crate::mux::focus_anchor::{FocusActionRequest, FocusIntentState, FocusOrigin};

    [true, false]
        .into_iter()
        .map(|earlier_ack_first| {
            let (mut rig, mut snapshot) = command_rig();
            let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
            sample_focus(&mut rig, &mut snapshot, pane(4));
            let walk = CardRequest::Walk {
                direction: CardDirection::Next,
                worktree: false,
            };
            assert_eq!(command(&mut rig, walk.clone()), CardReply::Done);
            let first = rig.view().ctx().focus_rollback.as_ref().unwrap().clone();
            rig.state.pane.clock.advance(Duration::from_millis(30));
            assert_eq!(command(&mut rig, walk.clone()), CardReply::Done);
            let second = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
            rig.state.pane.clock.advance(Duration::from_millis(10));
            let nonce = crate::mux::focus_anchor::request_action_with_test_sample(
                &rig.runtime,
                "rimz-test",
                FocusActionRequest {
                    pane_id: pane(1),
                    origin: FocusOrigin::User,
                    input_at_ms: Some(first.started_at_ms),
                    repair_generation: None,
                    expected_pre_action: None,
                    placement: None,
                    order: None,
                },
                || Ok(snapshot.client_views.clone()),
                || rig.state.pane.clock.now_ms(),
            )
            .unwrap();
            let mut earlier = crate::mux::focus_anchor::load(&rig.runtime).unwrap();
            if applied {
                earlier.state = FocusIntentState::Applied;
                earlier.applied_at_ms = Some(rig.state.pane.clock.now_ms());
            }
            crate::mux::focus_anchor::store(&rig.runtime, &earlier).unwrap();
            rig.state.fold_focus_intent(earlier);
            let first_ack = Wakeup::FocusDispatched {
                input: first.input,
                nonce,
            };
            let second_ack = Wakeup::FocusDispatched {
                input: second,
                nonce: crate::ids::FocusNonce::new(),
            };
            let acks = if earlier_ack_first {
                [first_ack, second_ack]
            } else {
                [second_ack, first_ack]
            };
            for ack in acks {
                rig.state.on_wakeup(&mut rig.terminal, ack).unwrap();
            }
            let pending = rig.view().ctx().pending_input_focus.clone();
            let selected = rig.view().ui().selected_pane.clone();
            assert_eq!(command(&mut rig, walk), CardReply::Done);
            let next = rig.view().ui().selected_pane.clone();
            [pending, selected, next]
        })
        .collect()
}

#[test]
fn card_walk_ignores_an_earlier_requested_anchor_dispatched_after_later_input() {
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    assert_eq!(
        delayed_earlier_walk(false),
        vec![[Some(pane(2)), Some(pane(2)), Some(pane(3))]; 2],
        "acknowledgements in input order and reverse order"
    );
}

#[test]
fn card_walk_ignores_an_earlier_applied_anchor_dispatched_after_later_input() {
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    assert_eq!(
        delayed_earlier_walk(true),
        vec![[Some(pane(2)), Some(pane(2)), Some(pane(3))]; 2],
        "acknowledgements in input order and reverse order"
    );
}

#[test]
fn card_focus_confirmation_rejects_observations_before_request() {
    use crate::mux::focus_anchor::{
        FocusActionRequest, FocusIntentState, FocusObservation, FocusOrigin,
    };
    use crate::sidebar::event_store::EventStore;
    use crate::sidebar::fuse::focus_intent_confirmed_from;

    let (mut rig, mut snapshot) = command_rig();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    sample_focus(&mut rig, &mut snapshot, target.clone());
    let input_at_ms = rig.state.pane.clock.now_ms();
    rig.state.pane.clock.advance(Duration::from_millis(20));
    let requested_at_ms = rig.state.pane.clock.now_ms();
    crate::mux::focus_anchor::request_action_with_test_sample(
        &rig.runtime,
        "rimz-test",
        FocusActionRequest {
            pane_id: target.clone(),
            origin: FocusOrigin::User,
            input_at_ms: Some(input_at_ms),
            repair_generation: None,
            expected_pre_action: None,
            placement: None,
            order: None,
        },
        || Ok(snapshot.client_views.clone()),
        || requested_at_ms,
    )
    .unwrap();
    let mut anchor = crate::mux::focus_anchor::load(&rig.runtime).unwrap();
    anchor.state = FocusIntentState::Applied;
    anchor.applied_at_ms = Some(requested_at_ms);
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    let anchor = crate::mux::focus_anchor::load(&rig.runtime).unwrap();
    let mut observed = FocusObservation::from_snapshot(&snapshot);
    let confirmed = [input_at_ms + 10, requested_at_ms, requested_at_ms + 1].map(|stamp| {
        observed.panes_observed_at_ms = Some(stamp);
        let sample = focus_intent_confirmed_from(&observed, &EventStore::default(), &anchor, stamp);
        observed.panes_observed_at_ms = None;
        let mut events = EventStore::default();
        events.append(
            SidebarEvent::FocusChanged {
                focused: vec![target.clone()],
                unfocused: Vec::new(),
            },
            stamp,
            stamp,
        );
        [
            sample,
            focus_intent_confirmed_from(&observed, &events, &anchor, stamp),
        ]
    });
    assert_eq!(
        confirmed,
        [[false, false], [true, true], [true, true]],
        "sample and event confirmation before, at, and after request time"
    );
}

fn card_pick_escapes(ack: bool) {
    use crate::mux::focus_anchor::{
        FOCUS_ANCHOR_FRESH, FocusAnchor, FocusIntentState, FocusOrigin,
    };

    let (mut rig, mut snapshot) = command_rig();
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(
        command(&mut rig, CardRequest::Slot { n: 1 }),
        CardReply::Done
    );
    if ack {
        let input = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
        let anchor = FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "rimz-test".into(),
            pane_id: pane(1),
            origin: FocusOrigin::User,
            repair_generation: None,
            input_at_ms: None,
            issued_at_ms: rig.state.pane.clock.now_ms(),
            applied_at_ms: Some(rig.state.pane.clock.now_ms()),
            state: FocusIntentState::Applied,
            pre_action: snapshot.client_views.clone(),
            placement: None,
            order: None,
        };
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        rig.state.fold_focus_intent(anchor.clone());
        rig.state
            .on_wakeup(
                &mut rig.terminal,
                Wakeup::FocusDispatched {
                    input,
                    nonce: anchor.nonce,
                },
            )
            .unwrap();
    }
    rig.state
        .pane
        .clock
        .advance(FOCUS_ANCHOR_FRESH - Duration::from_millis(1));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(rig.view().ctx().pending_input_focus, Some(pane(1)));
    rig.state.pane.clock.advance(Duration::from_millis(1));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(rig.view().ctx().pending_input_focus, None);
    assert!(rig.view().ctx().focus_rollback.is_none());
    assert!(rig.view().ui().browse.is_none());
    let selected = Some(pane(if ack { 1 } else { 4 }));
    assert_eq!(rig.view().ui().selected_pane, selected);
    if ack {
        assert!(crate::mux::focus_anchor::load(&rig.runtime).is_some());
        // With no native event, an applied anchor fences the unchanged pre-action sample.
        for advance in [Duration::from_millis(1), Duration::from_millis(3500)] {
            rig.state.pane.clock.advance(advance);
            sample_focus(&mut rig, &mut snapshot, pane(4));
            assert_eq!(rig.view().ctx().pending_input_focus, None);
            assert_eq!(rig.view().ui().selected_pane, selected);
            assert!(crate::mux::focus_anchor::load(&rig.runtime).is_some());
        }
        rig.event(SidebarEvent::FocusChanged {
            focused: vec![pane(1)],
            unfocused: vec![pane(4)],
        });
        assert_eq!(rig.view().ui().selected_pane, selected);
        assert!(crate::mux::focus_anchor::load(&rig.runtime).is_none());
    }
}

#[test]
fn card_pick_without_an_ack_escapes_after_the_focus_anchor_bound() {
    card_pick_escapes(false);
}

#[test]
fn card_pick_with_an_unconfirmed_anchor_escapes_after_the_focus_anchor_bound() {
    card_pick_escapes(true);
}

#[test]
fn card_answered_by_the_host_is_not_reduced_even_before_its_local_deadline() {
    let (mut rig, _) = command_rig();
    let (input, _) = queued(&rig, CardRequest::Slot { n: 2 });
    let Wakeup::Card(command) = &input else {
        unreachable!()
    };
    *command.answer.lock().unwrap() = Some(command.request.refused("sidebar busy; try again"));
    let before = rig.views.lock().selections_for_test();
    rig.state.on_input(input, &mut rig.terminal).unwrap();
    assert_eq!(rig.views.lock().selections_for_test(), before);
    assert_eq!(
        rig.state
            .pane
            .clock
            .focus_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn card_walk_intent_is_not_a_native_landing() {
    let (mut rig, mut snapshot) = command_rig();
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    let walk = CardRequest::Walk {
        direction: CardDirection::Next,
        worktree: false,
    };
    assert_eq!(command(&mut rig, walk.clone()), CardReply::Done);
    rig.state
        .fold_focus_intent(crate::mux::focus_anchor::FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "rimz-test".into(),
            pane_id: pane(1),
            origin: crate::mux::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            input_at_ms: None,
            issued_at_ms: rig.state.pane.clock.now_ms(),
            applied_at_ms: None,
            state: crate::mux::focus_anchor::FocusIntentState::Requested,
            pre_action: snapshot.client_views.clone(),
            placement: None,
            order: None,
        });
    assert_eq!(rig.view().ctx().pending_input_focus, Some(pane(1)));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(1)));
    assert_eq!(command(&mut rig, walk), CardReply::Done);
    assert_eq!(rig.view().ui().selected_pane, Some(pane(2)));
    sample_focus(&mut rig, &mut snapshot, pane(1));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(2)));
    sample_focus(&mut rig, &mut snapshot, pane(2));
    assert_eq!(rig.view().ctx().pending_input_focus, None);
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(4)));
}

#[test]
fn external_focus_supersedes_a_dispatched_card_pick_without_a_native_landing() {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusOrigin};

    for (delayed_ack, confirmed) in [(false, false), (true, false), (true, true), (false, true)] {
        let (mut rig, mut snapshot) = command_rig();
        let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
        sample_focus(&mut rig, &mut snapshot, pane(4));
        assert_eq!(
            command(&mut rig, CardRequest::Slot { n: 1 }),
            CardReply::Done
        );
        let input = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
        let mut anchor = FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "rimz-test".into(),
            pane_id: pane(1),
            origin: FocusOrigin::User,
            repair_generation: None,
            input_at_ms: None,
            issued_at_ms: rig.state.pane.clock.now_ms(),
            applied_at_ms: Some(rig.state.pane.clock.now_ms()),
            state: FocusIntentState::Applied,
            pre_action: snapshot.client_views.clone(),
            placement: None,
            order: None,
        };
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        rig.state.fold_focus_intent(anchor.clone());
        let dispatched = Wakeup::FocusDispatched {
            input,
            nonce: anchor.nonce,
        };
        if !delayed_ack {
            rig.state
                .on_wakeup(&mut rig.terminal, dispatched.clone())
                .unwrap();
        }
        if confirmed {
            rig.event(SidebarEvent::FocusChanged {
                focused: vec![pane(1)],
                unfocused: vec![pane(4)],
            });
        }
        sample_focus(&mut rig, &mut snapshot, pane(4));
        assert_eq!(rig.view().ui().selected_pane, Some(pane(1)));

        rig.state.pane.clock.advance(Duration::from_millis(1));
        anchor.nonce = crate::ids::FocusNonce::new();
        anchor.pane_id = pane(4);
        anchor.issued_at_ms = rig.state.pane.clock.now_ms();
        anchor.applied_at_ms = Some(anchor.issued_at_ms);
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        rig.state.fold_focus_intent(anchor);
        if delayed_ack {
            rig.state.on_wakeup(&mut rig.terminal, dispatched).unwrap();
        }
        assert_eq!(
            rig.view().ui().selected_pane,
            Some(pane(4)),
            "a newer external focus must release the dispatched pick: {delayed_ack}, {confirmed}"
        );
        assert_eq!(
            command(
                &mut rig,
                CardRequest::Walk {
                    direction: CardDirection::Next,
                    worktree: false,
                }
            ),
            CardReply::Done
        );
        assert_eq!(rig.view().ui().selected_pane, Some(pane(1)));
    }
}

#[test]
fn successful_focus_dispatch_reports_the_input_and_request_identity() {
    let pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let input = crate::ids::FocusNonce::new();
    let nonce = crate::ids::FocusNonce::new();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    finish_focus_request(
        &pane,
        Some(&send),
        Some(Ok(nonce)),
        None,
        Some(input),
        |_| Ok(true),
    );
    assert_eq!(
        receive.try_recv(),
        Ok(Wakeup::FocusDispatched { input, nonce })
    );
}

#[test]
fn card_superseded_dispatch_releases_only_its_pick_without_an_alert() {
    let (mut rig, mut snapshot) = command_rig();
    let pane = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    sample_focus(&mut rig, &mut snapshot, pane(4));
    let walk = CardRequest::Walk {
        direction: CardDirection::Next,
        worktree: false,
    };
    assert_eq!(command(&mut rig, walk.clone()), CardReply::Done);
    let first = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
    assert_eq!(command(&mut rig, walk), CardReply::Done);
    let second = rig.view().ctx().focus_rollback.as_ref().unwrap().input;
    for (input, expected) in [(first, Some(pane(2))), (second, None)] {
        rig.state
            .on_wakeup(&mut rig.terminal, Wakeup::FocusSuperseded { input })
            .unwrap();
        assert_eq!(rig.view().ctx().pending_input_focus, expected);
    }
    assert!(rig.view().ui().browse.is_none());
    assert!(rig.view().ctx().health.alert.is_none());
    sample_focus(&mut rig, &mut snapshot, pane(4));
    assert_eq!(rig.view().ui().selected_pane, Some(pane(4)));
}

#[test]
fn superseded_focus_worker_reports_the_input_identity() {
    let pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let input = crate::ids::FocusNonce::new();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    finish_focus_request(
        &pane,
        Some(&send),
        Some(Ok(crate::ids::FocusNonce::new())),
        None,
        Some(input),
        |_| Ok(false),
    );
    assert_eq!(receive.try_recv(), Ok(Wakeup::FocusSuperseded { input }));
}

#[test]
fn a_later_card_pick_supersedes_a_same_tab_worker_before_it_starts() {
    let (mut rig, _) = command_rig();
    rig.state.pane.clock = LoopClock::wall();
    rig.state.pane.clock.skip_focus = true;
    rig.state.pane.focus_peers = Some(rig.focus_plane.focus_peers(
        rig.state.pane.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    ));
    let walk = CardRequest::Walk {
        direction: CardDirection::Next,
        worktree: false,
    };
    let (first, _) = queued(&rig, walk.clone());
    rig.state.apply_input([first], &mut rig.terminal).unwrap();
    let first_worker = rig.state.pane.input_jump.take();
    let (second, _) = queued(&rig, walk);
    rig.state.apply_input([second], &mut rig.terminal).unwrap();
    let requested = first_worker
        .as_ref()
        .map_or(Some(17), |peers| peers.request(|| 17));
    assert_eq!(
        requested, None,
        "an earlier same-tab worker must not replace the later focus intent"
    );
}

#[test]
fn card_request_rechecks_queued_view_and_expiry_before_changing_any_view() {
    let (mut rig, _) = command_rig();
    let (input, reply) = queued(&rig, CardRequest::Slot { n: 2 });
    let now = rig.state.pane.clock.now();
    rig.views.lock().reconcile(
        &[
            ClientPaneView {
                client_id: MuxClientId::Zellij(1),
                pane_id: rig.state.pane.config.own_pane.clone(),
            },
            ClientPaneView {
                client_id: MuxClientId::Zellij(2),
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_100"),
            },
        ],
        true,
        now,
        Duration::from_secs(60),
    );
    let before = rig.views.lock().selections_for_test();
    rig.state.on_input(input, &mut rig.terminal).unwrap();
    assert!(matches!(reply.try_recv(), Ok(CardReply::Refused { .. })));
    assert_eq!(rig.views.lock().selections_for_test(), before);

    let (mut rig, _) = command_rig();
    let (input, reply) = queued(&rig, CardRequest::Slot { n: 2 });
    rig.state.pane.clock.advance(Duration::from_secs(6));
    let before = rig.views.lock().selections_for_test();
    rig.state.on_input(input, &mut rig.terminal).unwrap();
    assert!(matches!(reply.try_recv(), Ok(CardReply::Refused { .. })));
    assert_eq!(rig.views.lock().selections_for_test(), before);
}

#[test]
fn card_expired_request_does_not_drain_a_pending_fold() {
    let (mut rig, mut snapshot) = command_rig();
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    rig.view().ui().selected_pane = Some(first.clone());
    rig.view().ui().selected_index = Some(0);
    rig.view().ui().baseline_pane = Some(first);
    let (input, reply) = queued(&rig, CardRequest::Slot { n: 2 });
    rig.state.pane.clock.advance(Duration::from_secs(6));
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).clear();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: 0,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    });
    rig.send(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });
    let before = rig.views.lock().selections_for_test();
    let preparations = rig.state.pane.fold_preparations;
    rig.state.on_input(input, &mut rig.terminal).unwrap();
    assert!(matches!(reply.try_recv(), Ok(CardReply::Refused { .. })));
    assert_eq!(rig.views.lock().selections_for_test(), before);
    assert_eq!(
        rig.state.pane.fold_preparations, preparations,
        "an expired command cannot prepare or reduce a pending fold"
    );
}

#[test]
fn card_empty_filter_writes_nothing_and_no_history_notice_preserves_active_faults() {
    let (mut rig, _) = command_rig();
    rig.view().ui().make_up_filter.query = Some("main".into());
    let lens = rig.view().ui().make_up_filter.clone();
    crate::sidebar::body_filter::write(&rig.runtime, &lens).unwrap();
    let before = std::fs::read(rig.runtime.sidebar_filter_path()).unwrap();
    assert!(matches!(
        command(&mut rig, CardRequest::FilterNeedsYou { toggle: true }),
        CardReply::Refused { .. }
    ));
    assert_eq!(
        std::fs::read(rig.runtime.sidebar_filter_path()).unwrap(),
        before
    );
    assert!(matches!(
        command(&mut rig, CardRequest::Last),
        CardReply::Refused { .. }
    ));
    let notice = rig
        .view()
        .ctx()
        .health
        .alert
        .clone()
        .expect("resolved no-op notice");
    assert!(notice.reason.contains("no previous card"));
    assert!(notice.recovered_at.is_some());
    rig.view().ctx().health.alert = Some(render::Alert {
        reason: "pane read failed".into(),
        since: Timestamp::UNIX_EPOCH,
        recovered_at: None,
    });
    command(&mut rig, CardRequest::Last);
    assert_eq!(
        rig.view().ctx().health.alert.as_ref().unwrap().reason,
        "pane read failed"
    );
}

#[test]
fn card_failed_jump_restores_previous_card_with_the_baseline() {
    let (mut rig, _) = command_rig();
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let previous = PaneId::from_parts(MuxName::Zellij, "terminal_4");
    rig.view().ui().baseline_pane = Some(first.clone());
    rig.view().ui().previous_card = Some(previous.clone());
    assert_eq!(
        command(&mut rig, CardRequest::Slot { n: 2 }),
        CardReply::Done
    );
    {
        let mut guard = rig.view();
        let (view, context) = guard.pair();
        super::super::super::selection::reconcile_selection(
            view,
            &context.current,
            Some(second.clone()),
        );
        assert_eq!(view.previous_card, Some(first.clone()));
        assert!(
            context
                .roll_back_focus(view, &second, Instant::now())
                .is_some()
        );
        assert_eq!(view.baseline_pane, Some(first));
        assert_eq!(view.previous_card, Some(previous));
    }
}

#[test]
fn close_and_palette_keys_spawn_their_rimz_commands_for_the_selected_card() {
    let mut rig = Rig::new();
    rig.view().ctx().current = agent_snapshot(&rig.ws);
    let card = rig
        .state
        .views
        .mirror()
        .rows()
        .next()
        .unwrap()
        .pane
        .clone()
        .unwrap()
        .pane_id;
    rig.view().ui().selected_index = Some(0);
    rig.view().ui().selected_pane = Some(card.clone());
    let room = rig.state.pane.config.room.session_name.clone();
    rig.input(KeyAction::Close);
    let modes: [&[&str]; 4] = [
        &["--mode", "commands"],
        &["--mode", "new"],
        &["--mode", "new", "--new-worktree"],
        &["--repeat"],
    ];
    for key in [':', 'o', 'O', '.'] {
        let press = Wakeup::Press {
            code: ratatui::crossterm::event::KeyCode::Char(key),
            mods: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        rig.state.on_wakeup(&mut rig.terminal, press).unwrap();
    }
    let mut expected = vec![super::super::super::key_spawn::close_argv(
        &card,
        &room,
        MuxName::Tmux,
    )];
    expected.extend(modes.map(|mode| {
        let mode: Vec<String> = mode.iter().map(|&arg| arg.to_owned()).collect();
        super::super::super::key_spawn::palette_argv(&mode, &room, MuxName::Tmux, Some(&card), None)
    }));
    assert_eq!(
        super::super::super::key_spawn::take_spawned(),
        expected,
        "no client views the pane, so the palette names none"
    );

    let clock = LoopClock::play("2026-06-09T12:00:00Z".parse().unwrap());
    let mut played = Rig::with_clock(None, BodyLens::default(), None, clock);
    played.view().ctx().current = agent_snapshot(&played.ws);
    played.view().ui().selected_index = Some(0);
    played.view().ui().selected_pane = Some(card);
    played.input(KeyAction::Close);
    assert!(
        super::super::super::key_spawn::take_spawned().is_empty(),
        "a play clock runs no command"
    );
}
