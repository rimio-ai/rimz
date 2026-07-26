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
fn width_target_event_reloads_the_target_without_a_producer_fetch() {
    let mut rig = Rig::new();
    crate::sidebar::width_target::pin(
        &rig.runtime,
        std::num::NonZeroU16::new(90).expect("nonzero width"),
        200,
    )
    .expect("pin width target");

    rig.event(SidebarEvent::WidthTargetChanged);

    assert_eq!(
        rig.state.width_control.max_legit_cols(),
        72,
        "the share stays unresolved until backend view geometry arrives",
    );
    assert!(
        rig.next_request().is_none(),
        "width propagation stays out of the producer path",
    );
}

#[test]
fn birth_seeds_the_shared_body_filter() {
    let rig = Rig::with_filter(BodyFilter::Unread);

    assert_eq!(rig.state.ui.make_up_filter, Some(BodyFilter::Unread));
}

#[test]
fn body_filter_event_adopts_the_shared_file_and_repaints() {
    let mut rig = Rig::new();
    rig.state.current = agent_snapshot(&rig.ws);
    rig.state.dirty = false;
    let filter = BodyFilter::Status(crate::agents::AgentStatus::Idle);
    crate::sidebar::body_filter::write(&rig.runtime, filter).expect("write shared filter");

    rig.event(SidebarEvent::BodyFilterChanged);

    assert_eq!(rig.state.ui.make_up_filter, Some(filter));
    assert!(rig.state.dirty);
    assert!(
        rig.next_request().is_none(),
        "filter propagation stays out of the producer path"
    );
}

#[test]
fn successful_fetch_converges_a_missed_body_filter_event() {
    let mut rig = Rig::new();
    let filter = BodyFilter::Status(crate::agents::AgentStatus::Idle);
    crate::sidebar::body_filter::write(&rig.runtime, filter).expect("write shared filter");

    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot, true);

    assert_eq!(rig.state.ui.make_up_filter, Some(filter));
}

#[test]
fn failed_or_rowless_birth_fold_does_not_publish_a_filter_clear() {
    let filter = BodyFilter::Status(crate::agents::AgentStatus::Waiting);
    let mut rig = Rig::with_filter(filter);

    rig.deliver(FetchUpdate::Failed {
        error: "not ready".to_owned(),
        role: FetchRole::Producer,
        pane_frame: PaneFrame::Held,
    });
    assert_eq!(rig.state.ui.make_up_filter, None);
    assert_eq!(
        crate::sidebar::body_filter::load(&rig.runtime),
        Some(filter)
    );

    let rowless = snapshot(&rig.ws);
    rig.fold(rowless, true);
    assert_eq!(rig.state.ui.make_up_filter, None);
    assert_eq!(
        crate::sidebar::body_filter::load(&rig.runtime),
        Some(filter)
    );
}

#[test]
fn empty_body_filter_auto_clear_updates_the_shared_file() {
    let filter = BodyFilter::Status(crate::agents::AgentStatus::Waiting);
    let mut rig = Rig::with_filter(filter);
    assert_eq!(
        crate::sidebar::body_filter::load(&rig.runtime),
        Some(filter)
    );

    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot, true);

    assert_eq!(rig.state.ui.make_up_filter, None);
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), None);
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
