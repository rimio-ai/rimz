//! Which sidebar events provoke a producer fetch, when, and how a burst
//! coalesces into one.

use super::*;

#[test]
fn unchanged_fetch_outcome_clears_in_flight_without_dirtying_frame() {
    let mut rig = Rig::new();
    rig.state.dirty = false;
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());

    rig.deliver(FetchUpdate::Unchanged {
        role: FetchRole::Producer,
    });

    assert!(!rig.state.dirty);
    rig.fetch.request(FetchRequest::default(), false);
    assert!(
        rig.next_request().is_some(),
        "unchanged final outcome must release the single-flight request"
    );
}

#[test]
fn unchanged_fetch_outcome_dispatches_queued_refetch() {
    let mut rig = Rig::new();
    rig.state.dirty = false;
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    rig.fetch
        .request(FetchRequest::producer_fresh_panes(), true);

    rig.deliver(FetchUpdate::Unchanged {
        role: FetchRole::Producer,
    });

    assert!(!rig.state.dirty);
    assert!(
        rig.next_request()
            .expect("pending refetch dispatched")
            .is_producer_fresh_panes(),
        "unchanged final outcome must not strand a queued forced refetch"
    );
}

#[test]
fn unwatched_consumer_coalesces_identity_free_fetches_until_clamp_deadline() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.hide_consumer();

    for event in [store_delta(), SidebarEvent::PanesChanged, store_delta()] {
        rig.event(event);
    }

    assert!(
        rig.next_request().is_none(),
        "unwatched consumer defers the burst"
    );
    assert!(
        rig.fetch
            .deferred_request()
            .expect("pending fetch")
            .is_producer_fresh_panes(),
        "coalescing preserves the strongest freshness requirement"
    );
    rig.fetch.defer_until(
        FetchRequest::default(),
        Instant::now() - Duration::from_millis(1),
    );

    rig.maintenance();

    assert!(
        rig.next_request()
            .expect("one deferred fetch")
            .is_producer_fresh_panes()
    );
    assert!(rig.next_request().is_none(), "burst emits one fetch");
}

#[test]
fn lifecycle_store_delta_preserves_fresh_pane_verification() {
    for signal in [
        crate::agents::LifecycleSignal::Registered.tag(),
        crate::agents::LifecycleSignal::Ended.tag(),
    ] {
        let mut rig = Rig::new();
        rig.state.last_known_elder = true;

        rig.event(SidebarEvent::StoreDelta {
            event_method: Some(crate::store::event::AGENT_LIFECYCLE_METHOD.to_owned()),
            agent_signal: Some(signal.to_owned()),
        });

        let request = rig.next_request().expect("immediate lifecycle fetch");
        assert!(request.is_producer_fresh_panes(), "signal: {signal}");
    }
}

#[test]
fn repeated_hidden_metrics_publications_fold_once_at_the_background_deadline() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.hide_consumer();

    for _ in 0..3 {
        rig.event(pane_publication(
            crate::sidebar::events::PaneFramePublicationKind::Metrics,
        ));
    }
    assert!(rig.next_request().is_none());
    let deadline = rig.fetch.next_deadline().expect("one deferred fetch");
    assert!(
        deadline.saturating_duration_since(Instant::now())
            <= crate::sidebar::timing::UNWATCHED_METRICS_FOLD_CLAMP
    );
    rig.fetch.defer_until(
        FetchRequest::default(),
        Instant::now() - Duration::from_millis(1),
    );

    rig.maintenance();

    assert!(
        rig.next_request().is_some(),
        "the metrics burst emits one fetch at its deadline"
    );
    assert!(rig.next_request().is_none());
    assert!(rig.fetch.next_deadline().is_none());
}

#[test]
fn topology_and_store_publications_shorten_a_metrics_deadline() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for shorter in [
        pane_publication(crate::sidebar::events::PaneFramePublicationKind::Topology),
        store_delta(),
    ] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide_consumer();
        rig.event(pane_publication(
            crate::sidebar::events::PaneFramePublicationKind::Metrics,
        ));
        let metrics_due = rig.fetch.next_deadline().expect("metrics pending");

        rig.event(shorter);
        let shortened = rig.fetch.next_deadline().expect("shortened pending fetch");
        assert!(shortened < metrics_due);

        rig.event(SidebarEvent::PanesChanged);
        assert_eq!(rig.fetch.next_deadline(), Some(shortened));
        assert!(
            rig.fetch
                .deferred_request()
                .expect("merged pending fetch")
                .is_producer_fresh_panes()
        );
        assert!(rig.next_request().is_none());
    }
}

#[test]
fn watched_metrics_and_hidden_presence_publications_fold_immediately() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for (publication, watched) in [
        (
            crate::sidebar::events::PaneFramePublicationKind::Metrics,
            true,
        ),
        (
            crate::sidebar::events::PaneFramePublicationKind::Presence,
            false,
        ),
    ] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide_consumer();
        if watched {
            rig.watch();
        }

        rig.event(pane_publication(publication));

        assert!(rig.next_request().is_some());
        assert!(rig.fetch.next_deadline().is_none());
    }
}

#[test]
fn watched_renderer_and_elder_fetch_identity_free_events_immediately() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for (watched, elder) in [(true, false), (false, true)] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide_consumer();
        rig.state.last_known_elder = elder;
        if watched {
            rig.watch();
        }

        rig.event(store_delta());

        assert!(rig.next_request().is_some());
        assert!(rig.fetch.next_deadline().is_none());
    }
}

#[test]
fn maintenance_watchdog_absorbs_deferred_unwatched_fetch() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.hide_consumer();

    rig.event(store_delta());
    assert!(rig.fetch.next_deadline().is_some());

    rig.state.last_self_close_check = Instant::now() - SELF_CLOSE_WATCHDOG;
    rig.maintenance();

    assert!(
        rig.next_request().is_some(),
        "watchdog dispatches one fetch"
    );
    assert!(rig.fetch.next_deadline().is_none());
    assert!(
        rig.next_request().is_none(),
        "the deferred nudge merges into the watchdog fetch"
    );
}

#[test]
fn focus_resume_flushes_pending_metrics_fetch() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.hide_consumer();

    rig.event(pane_publication(
        crate::sidebar::events::PaneFramePublicationKind::Metrics,
    ));
    assert!(rig.fetch.next_deadline().is_some());

    rig.event(SidebarEvent::FocusChanged {
        focused: vec![own_pane],
        unfocused: Vec::new(),
    });

    assert!(rig.fetch.next_deadline().is_none());
    assert!(
        rig.next_request()
            .expect("focus flushed pending fetch")
            .is_producer_fresh_panes()
    );
}

#[test]
fn width_target_event_reloads_the_override_without_a_producer_fetch() {
    let mut rig = Rig::new();
    crate::sidebar::width_override::write(
        &rig.runtime,
        std::num::NonZeroU16::new(90).expect("nonzero width"),
    )
    .expect("write width override");

    rig.event(SidebarEvent::WidthTargetChanged);

    assert_eq!(rig.state.width_control.max_legit_cols(), 90);
    assert!(
        rig.next_request().is_none(),
        "width propagation stays out of the producer path",
    );
}

#[test]
fn focus_out_closes_help_popup() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    let snapshot = snapshot_with_focused_pane(&rig.ws, own_pane.clone());
    rig.set_pulled(&snapshot);
    rig.state.current = snapshot;
    rig.state.ui.help_visible = true;
    rig.state.optimistic_watch_until = Some(Instant::now() + Duration::from_secs(1));

    rig.event(SidebarEvent::FocusChanged {
        focused: Vec::new(),
        unfocused: vec![own_pane],
    });

    assert!(!rig.state.ui.help_visible);
    assert!(rig.state.optimistic_watch_until.is_none());
}
