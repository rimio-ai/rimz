use super::*;
use crate::app::fixtures::workspace;
use rimz::SidebarInstanceId;

#[test]
fn producer_skips_the_fork_while_its_frame_is_within_one_tick() {
    // The two-speed contract: a ledger-delta storm paints per delta off the
    // in-process fast lane, forking at most once per data tick.
    assert!(!produce_this_cycle(true, false, Some(100), 1000));
    assert!(produce_this_cycle(true, false, Some(1000), 1000));
    assert!(
        produce_this_cycle(true, false, None, 1000),
        "no usable frame (cold start) always produces"
    );
}

#[test]
fn forced_requests_always_fork() {
    assert!(produce_this_cycle(true, true, Some(0), 1000));
    assert!(
        produce_this_cycle(false, true, Some(0), 1000),
        "a consumer reload/resize forks regardless of election"
    );
}

#[test]
fn consumer_never_forks_unforced_however_stale_the_frame() {
    // The storm-removal contract: staleness recovery belongs to the election
    // (the next-eldest becomes the producer within one heartbeat TTL), so a
    // wedged producer never turns every consumer into its own uncached
    // `list-panes` + git produce. The consumer keeps folding the held panes
    // with the event-fresh rollup — status stays live, only pane presence ages.
    assert!(!produce_this_cycle(false, false, Some(5_000), 1000));
    assert!(!produce_this_cycle(false, false, Some(60_000), 1000));
    assert!(
        !produce_this_cycle(false, false, None, 1000),
        "even a missing frame waits for the elected producer"
    );
}

#[test]
fn fetch_request_sends_immediately_when_idle() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut in_flight = false;
    let mut pending = None;
    let request = FetchRequest::fresh_panes();

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    assert!(in_flight);
    assert!(rx.try_recv().unwrap().force_produce);
    assert!(pending.is_none());
}

#[test]
fn fetch_request_preserves_forced_pane_refresh_while_in_flight() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut in_flight = true;
    let mut pending = Some(FetchRequest::default());
    let request = FetchRequest::fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    let pending = pending.expect("pending refetch");
    assert!(pending.force_produce);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);
}

#[test]
fn self_close_probe_request_sends_when_idle() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut in_flight = false;
    let mut pending = None;

    request_self_close_probe(&tx, &mut in_flight, &mut pending, Duration::ZERO);

    assert!(in_flight);
    assert_eq!(
        rx.try_recv().unwrap(),
        SelfCloseProbeRequest {
            delay: Duration::ZERO
        }
    );
    assert_eq!(pending, None);
}

#[test]
fn self_close_probe_request_coalesces_to_shortest_pending_delay() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut in_flight = true;
    let mut pending = Some(Duration::from_secs(2));

    request_self_close_probe(&tx, &mut in_flight, &mut pending, Duration::from_millis(50));

    assert!(in_flight);
    assert_eq!(pending, Some(Duration::from_millis(50)));
}

#[test]
fn self_close_probe_outcome_uses_the_existing_latch() {
    let config = ServeConfig {
        workspace_id: workspace(),
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 2,
        rimz_bin: PathBuf::from("rimz"),
    };
    let mut state = SelfCloseState::default();

    assert!(!apply_self_close_probe_outcome(
        &config,
        SelfCloseProbeOutcome {
            sibling_count: Some(1),
            error: None,
        },
        &mut state,
    ));
    assert!(state.seen_sibling);
    assert!(apply_self_close_probe_outcome(
        &config,
        SelfCloseProbeOutcome {
            sibling_count: Some(0),
            error: None,
        },
        &mut state,
    ));
}
