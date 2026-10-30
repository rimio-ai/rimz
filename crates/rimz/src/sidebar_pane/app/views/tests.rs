use super::*;
use crate::ids::MuxClientId;
use crate::sidebar_pane::app::clock::LoopClock;

const HOLD: Duration = Duration::from_secs(10);

fn assert_record(handle: &ViewsHandle) {
    let record = handle.read();
    let views = handle.lock();
    assert_eq!(
        record.watched(),
        !views.panes.is_empty() || views.resolved_panes().next().is_some()
    );
    assert_eq!(record.revision, views.revision);
    assert_eq!(record.fold.is_some(), views.fold.is_some());
    if let (Some(record), Some(locked)) = (&record.fold, &views.fold) {
        assert!(Arc::ptr_eq(record, locked));
    }
    for id in [10, 11, 20, 21, 99] {
        let pane = pane(id);
        assert_eq!(
            record.mapped(&pane),
            views.panes.contains_key(&pane),
            "{pane}"
        );
        assert_eq!(record.resolved(&pane), views.resolved(&pane), "{pane}");
        assert_eq!(record.observed(&pane), views.observed(&pane), "{pane}");
        let outcomes = |latest: &Latest| {
            latest
                .unseen(0)
                .map(|update| {
                    (
                        update.is_success(),
                        update.is_final(),
                        update
                            .preview_for(&pane)
                            .map(|(own, viewed)| (own, viewed.to_vec())),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(outcomes(&record.latest), outcomes(&views.latest));
    }
}

#[test]
fn room_watched_tracks_the_last_optimistically_mapped_pane() {
    let clock = clock();
    let handle = ViewsHandle::default();
    assert!(!handle.read().watched());
    handle.lock().presentation(&pane(10), clock.now(), true);
    assert!(handle.read().watched(), "optimism needs no resolved client");
    handle.lock().unpaint(&pane(10), clock.now());
    assert!(!handle.read().watched(), "the last mapped pane left");
}

#[test]
fn room_watched_includes_resolved_clients_without_a_mapped_pane() {
    let clock = clock();
    let handle = ViewsHandle::default();
    sample(&mut handle.lock(), &clock, &[(1, 10)]);
    handle.lock().panes.clear();
    assert!(
        handle.read().watched(),
        "resolved clients still watch the room"
    );
    sample(&mut handle.lock(), &clock, &[]);
    assert!(!handle.read().watched());
}

#[test]
fn record_follows_every_views_writer() {
    let clock = clock();
    let handle = ViewsHandle::default();
    assert_record(&handle);
    let source = handle.lock().presentation(&pane(10), clock.now(), false).0;
    assert_record(&handle);
    handle.lock().land(source, &pane(20), clock.now());
    assert_record(&handle);
    handle.lock().unpaint(&pane(20), clock.now());
    assert_record(&handle);
    sample(&mut handle.lock(), &clock, &[(1, 10)]);
    assert_record(&handle);
    sample(&mut handle.lock(), &clock, &[]);
    assert_record(&handle);
    clock.advance(HOLD);
    handle.lock().expire(clock.now(), HOLD);
    assert_record(&handle);
}

#[test]
fn publication_reconciles_before_any_reader_and_records_latest_outcomes() {
    let clock = clock();
    let handle = ViewsHandle::default();
    let mut snapshot = crate::sidebar_pane::app::fixtures::snapshot(
        &crate::sidebar_pane::app::fixtures::workspace(),
    );
    snapshot.viewed_panes = vec![pane(11)];
    let fold = Arc::new(WorkspaceFold {
        sampled_at: clock.now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
        frame: Some(Arc::new(frame(&[(1, 11)]))),
    });
    handle.lock().publish(
        FetchUpdate::Fold {
            fold,
            phase: super::super::fetch::FetchPhase::Final,
            source: super::super::fetch::SnapshotSource::Produced,
        },
        clock.now(),
    );
    assert!(
        handle.read().resolved(&pane(10)),
        "publication reconciles membership"
    );
    assert_record(&handle);
    handle.lock().publish(
        FetchUpdate::Failed {
            error: "offline".into(),
        },
        clock.now(),
    );
    assert_record(&handle);
}

fn clock() -> LoopClock {
    LoopClock::play("2026-06-01T00:00:00Z".parse().unwrap())
}

fn pane(id: u32) -> PaneId {
    PaneId::from_parts(crate::MuxName::Zellij, format!("terminal_{id}"))
}

fn sample(views: &mut Views, clock: &LoopClock, clients: &[(u32, u32)]) {
    let clients: Vec<_> = clients
        .iter()
        .map(|&(client, target)| ClientPaneView {
            client_id: MuxClientId::Zellij(client),
            pane_id: pane(target),
        })
        .collect();
    views.reconcile(&clients, true, clock.now(), HOLD);
}

#[test]
fn colocated_clients_split_and_rejoin_the_destination_view() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 12;
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 12);
    views.input(&pane(20), clock.now()).scroll_offset = 25;
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 12);
    sample(&mut views, &clock, &[(1, 20), (2, 20)]);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 25);
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 25);
}

#[test]
fn arrival_keeps_evidence_input_despite_newer_coviewer_input() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10)]);
    let (source, entry) = views.presentation(&pane(10), clock.now(), true);
    entry.state.scroll_offset = 11;
    let (evidence, entry) = views.presentation(&pane(20), clock.now(), true);
    entry.state.scroll_offset = 22;
    assert_ne!(source, evidence);
    views.input(&pane(10), clock.now()).scroll_offset = 33;
    sample(&mut views, &clock, &[(1, 20), (2, 10)]);
    let (arrived, entry) = views.presentation(&pane(20), clock.now(), false);
    assert_eq!((arrived, entry.state.scroll_offset), (evidence, 22));
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 33);
}

#[test]
fn simultaneous_swaps_carry_each_clients_view_without_joining() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 7;
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    views.input(&pane(20), clock.now()).scroll_offset = 19;
    sample(&mut views, &clock, &[(1, 20), (2, 10)]);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 7);
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 19);
}

#[test]
fn a_group_move_keeps_its_hold_and_animation_epoch() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10)]);
    let state = views.input(&pane(10), clock.now());
    state.scroll_offset = 8;
    state.animation_phase = 123;
    state.freeze = Some(crate::sidebar_pane::render::LayoutFreeze {
        frozen: Default::default(),
        armed_ms: 500,
    });
    clock.advance(Duration::from_secs(1));
    sample(&mut views, &clock, &[(1, 20), (2, 20)]);
    let state = views.paint(&pane(20), clock.now());
    assert_eq!(state.scroll_offset, 8);
    assert_eq!(state.animation_phase, 123);
    assert_eq!(state.freeze.as_ref().map(|hold| hold.armed_ms), Some(500));
}

#[test]
fn departure_retains_a_frozen_copy_not_the_survivors_later_input() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 8;
    sample(&mut views, &clock, &[(1, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 21;
    clock.advance(HOLD - Duration::from_millis(1));
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 8);
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 21);
}

#[test]
fn retention_expires_at_the_deadline_not_one_tick_before() {
    for elapsed in [HOLD - Duration::from_millis(1), HOLD] {
        let clock = clock();
        let mut views = Views::default();
        sample(&mut views, &clock, &[(1, 10)]);
        views.input(&pane(10), clock.now()).scroll_offset = 17;
        sample(&mut views, &clock, &[]);
        clock.advance(elapsed);
        sample(&mut views, &clock, &[(1, 10)]);
        assert_eq!(
            views.paint(&pane(10), clock.now()).scroll_offset,
            if elapsed < HOLD { 17 } else { 0 },
            "elapsed {elapsed:?}"
        );
    }
}

#[test]
fn joining_an_incumbent_view_counts_as_arrival_for_recency() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 20), (3, 30)]);
    views.input(&pane(10), clock.now()).scroll_offset = 11;
    views.input(&pane(30), clock.now()).scroll_offset = 33;
    sample(&mut views, &clock, &[(1, 10), (2, 10), (3, 30)]);
    sample(&mut views, &clock, &[(1, 10), (2, 10), (3, 30), (4, 40)]);
    assert_eq!(views.paint(&pane(40), clock.now()).scroll_offset, 11);
}

#[test]
fn a_new_client_takes_the_most_recent_view_and_repeated_samples_do_not_touch_recency() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    views.input(&pane(20), clock.now()).scroll_offset = 22;
    views.input(&pane(10), clock.now()).scroll_offset = 11;
    for _ in 0..3 {
        clock.advance(Duration::from_millis(1));
        sample(&mut views, &clock, &[(1, 10), (2, 20)]);
        views.paint(&pane(20), clock.now());
    }
    sample(&mut views, &clock, &[(1, 10), (2, 20), (3, 30)]);
    assert_eq!(views.paint(&pane(30), clock.now()).scroll_offset, 11);
    views.input(&pane(30), clock.now()).scroll_offset = 33;
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 11);
}

#[test]
fn unresolved_empty_or_retained_samples_neither_depart_nor_reset_clients() {
    for retained in [false, true] {
        let clock = clock();
        let mut views = Views::default();
        sample(&mut views, &clock, &[(1, 10), (2, 20)]);
        views.input(&pane(10), clock.now()).scroll_offset = 11;
        views.input(&pane(20), clock.now()).scroll_offset = 22;
        let stale = if retained {
            vec![ClientPaneView {
                client_id: MuxClientId::Zellij(1),
                pane_id: pane(10),
            }]
        } else {
            Vec::new()
        };
        views.reconcile(&stale, false, clock.now(), HOLD);
        clock.advance(HOLD + Duration::from_millis(1));
        sample(&mut views, &clock, &[(1, 20), (2, 10)]);
        assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 11);
        assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 22);
    }
}

#[test]
fn evidence_does_not_take_another_painting_panes_view() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    views.input(&pane(10), clock.now()).scroll_offset = 11;
    views.input(&pane(20), clock.now()).scroll_offset = 22;
    views.input(&pane(30), clock.now()).scroll_offset = 33;
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 11);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 22);
}

#[test]
fn evidence_does_not_take_the_sole_resolved_clients_painting_view() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 11;
    views.input(&pane(20), clock.now()).scroll_offset = 22;
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 11);
    assert_eq!(views.clients[&MuxClientId::Zellij(1)].pane, Some(pane(10)));
}

#[test]
fn resolved_arrival_keeps_input_from_before_the_client_sample() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 30)]);
    views.input(&pane(10), clock.now()).scroll_offset = 11;
    views.input(&pane(20), clock.now()).scroll_offset = 22;
    sample(&mut views, &clock, &[(1, 20), (2, 30)]);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 22);
}

#[test]
fn a_new_client_does_not_mutate_a_departed_clients_held_copy() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 8;
    sample(&mut views, &clock, &[]);
    sample(&mut views, &clock, &[(2, 20)]);
    views.input(&pane(20), clock.now()).scroll_offset = 21;
    sample(&mut views, &clock, &[(1, 30), (2, 20)]);
    assert_eq!(views.paint(&pane(30), clock.now()).scroll_offset, 8);
}

#[test]
fn anonymous_input_does_not_mutate_a_departed_clients_held_copy() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 8;
    sample(&mut views, &clock, &[]);
    views.input(&pane(20), clock.now()).scroll_offset = 21;
    sample(&mut views, &clock, &[(1, 30)]);
    assert_eq!(views.paint(&pane(30), clock.now()).scroll_offset, 8);
}

#[test]
fn anonymous_views_expire_without_a_new_client_sample() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[]);
    views.input(&pane(10), clock.now()).scroll_offset = 17;
    views.unpaint(&pane(10), clock.now());
    clock.advance(HOLD);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 0);
}

#[test]
fn an_anonymous_painted_view_outlives_retention() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[]);
    views.input(&pane(10), clock.now()).scroll_offset = 17;
    clock.advance(HOLD + Duration::from_millis(1));
    assert_eq!(views.paint(&pane(10), clock.now()).scroll_offset, 17);
}

#[test]
fn reconciliation_holds_an_unpainted_anonymous_view_from_its_departure() {
    for elapsed in [HOLD - Duration::from_millis(1), HOLD] {
        let clock = clock();
        let mut views = Views::default();
        sample(&mut views, &clock, &[(1, 10)]);
        views.input(&pane(20), clock.now()).scroll_offset = 17;
        clock.advance(HOLD - Duration::from_millis(1));
        sample(&mut views, &clock, &[(1, 30)]);
        clock.advance(elapsed);
        assert_eq!(
            views.paint(&pane(20), clock.now()).scroll_offset,
            if elapsed < HOLD { 17 } else { 0 },
            "elapsed {elapsed:?}"
        );
    }
}

#[test]
fn simultaneous_arrivals_break_ties_before_any_arrival_bumps_recency() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10), (3, 10)]);
    views.input(&pane(10), clock.now()).scroll_offset = 8;
    sample(&mut views, &clock, &[(1, 10), (3, 10)]);
    // Geometry write-back does not count as client activity.
    views.paint(&pane(10), clock.now()).scroll_offset = 11;
    sample(&mut views, &clock, &[(1, 20), (2, 30), (3, 30)]);
    assert_eq!(views.paint(&pane(20), clock.now()).scroll_offset, 11);
    assert_eq!(views.paint(&pane(30), clock.now()).scroll_offset, 8);
}

#[test]
fn a_landing_holds_the_displaced_anonymous_view_from_its_departure() {
    for elapsed in [HOLD - Duration::from_millis(1), HOLD] {
        let clock = clock();
        let mut views = Views::default();
        sample(&mut views, &clock, &[]);
        views.input(&pane(10), clock.now()).scroll_offset = 17;
        let displaced = views.panes[&pane(10)];
        views.input(&pane(20), clock.now()).scroll_offset = 29;
        let source = views.panes[&pane(20)];
        clock.advance(HOLD - Duration::from_millis(1));
        views.land(source, &pane(10), clock.now());
        clock.advance(elapsed);
        views.refresh(clock.now());
        assert_eq!(
            views
                .entries
                .get(&displaced)
                .map(|entry| entry.state.scroll_offset),
            (elapsed < HOLD).then_some(17),
            "elapsed {elapsed:?}"
        );
    }
}

fn frame(clients: &[(u32, u32)]) -> PaneFrame {
    use crate::sidebar::frame::assemble_frame;
    let mut panes = Vec::new();
    for (sidebar, working) in [(10, 11), (10, 12), (20, 21)] {
        if !panes
            .iter()
            .any(|p: &crate::pane::PaneRef| p.pane_id == pane(sidebar))
        {
            let mut sidebar_pane = crate::sidebar_pane::app::fixtures::pane(
                &format!("terminal_{sidebar}"),
                &format!("tab_{sidebar}"),
                false,
            );
            sidebar_pane.command = Some("rimz-sidebar".into());
            panes.push(sidebar_pane);
        }
        panes.push(crate::sidebar_pane::app::fixtures::pane(
            &format!("terminal_{working}"),
            &format!("tab_{sidebar}"),
            false,
        ));
    }
    let clients: Vec<_> = clients
        .iter()
        .map(|&(client, focused)| ClientPaneView {
            client_id: MuxClientId::Zellij(client),
            pane_id: pane(focused),
        })
        .collect();
    let mut frame = assemble_frame(panes, 1, "rimz-test");
    frame.client_views = clients;
    frame.client_view_fresh = true;
    frame
}

#[test]
fn focus_and_parking_are_derived_only_from_the_views_clients() {
    let clock = clock();
    let mut views = Views::default();
    views.reconcile_frame(&frame(&[(1, 11), (2, 20)]), clock.now(), HOLD);
    assert_eq!(views.focused(&pane(10)), Some(pane(11)));
    assert!(!views.parked(&pane(10)));
    assert!(views.parked(&pane(20)));
    assert_eq!(
        views.focused(&pane(20)),
        None,
        "sidebar focus holds the baseline"
    );
}

#[test]
fn disagreeing_coviewers_hold_the_baseline_without_affecting_another_view() {
    let clock = clock();
    let mut views = Views::default();
    views.reconcile_frame(&frame(&[(1, 11), (2, 12), (3, 21)]), clock.now(), HOLD);
    assert_eq!(views.focused(&pane(10)), None);
    assert_eq!(views.focused(&pane(20)), Some(pane(21)));
    views.reconcile_frame(&frame(&[(1, 11), (2, 21), (3, 21)]), clock.now(), HOLD);
    assert_eq!(views.focused(&pane(10)), Some(pane(11)));
}

#[test]
fn a_split_copies_the_runtime_context_and_animation_epoch() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10)]);
    let initial = crate::sidebar_pane::app::fixtures::snapshot(
        &crate::sidebar_pane::app::fixtures::workspace(),
    );
    let epoch = clock.now();
    let context = views.context(&pane(10), clock.now(), initial.clone());
    context.health.failure_streak = 2;
    context.confirmed_focus_intent_ms = 42;
    clock.advance(Duration::from_secs(1));
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    let copy = views.context(&pane(20), clock.now(), initial.clone());
    assert_eq!(copy.anim_start, epoch);
    assert_eq!(copy.health.failure_streak, 2);
    assert_eq!(copy.confirmed_focus_intent_ms, 42);
    copy.health.failure_streak = 7;
    assert_eq!(
        views
            .context(&pane(10), clock.now(), initial)
            .health
            .failure_streak,
        2
    );
}

#[test]
fn a_departed_clients_context_is_frozen_beside_surviving_coviewers() {
    let clock = clock();
    let mut views = Views::default();
    sample(&mut views, &clock, &[(1, 10), (2, 10)]);
    let initial = crate::sidebar_pane::app::fixtures::snapshot(
        &crate::sidebar_pane::app::fixtures::workspace(),
    );
    views
        .context(&pane(10), clock.now(), initial.clone())
        .health
        .failure_streak = 2;
    sample(&mut views, &clock, &[(1, 10)]);
    views
        .context(&pane(10), clock.now(), initial.clone())
        .health
        .failure_streak = 7;
    sample(&mut views, &clock, &[(1, 10), (2, 20)]);
    assert_eq!(
        views
            .context(&pane(20), clock.now(), initial)
            .health
            .failure_streak,
        2
    );
}
