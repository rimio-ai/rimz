use super::*;
use crate::app::fixtures::{pane, workspace};
use rimz::{MuxName, SidebarInstanceId};

#[test]
fn produce_guard_maps_an_error_to_a_degraded_outcome() {
    let mut cursor = RollupCursor::new();
    let result = run_produce_guarded(&mut cursor, |_| {
        Err(rimz::sidebar::produce::ProduceErr::Fixture {
            path: PathBuf::from("/nonexistent/panes.json"),
            reason: "injected failure".to_owned(),
        })
    });
    assert!(result.unwrap_err().contains("injected failure"));
}

#[test]
fn produce_guard_maps_a_panic_to_a_degraded_outcome() {
    // Silence the default hook's backtrace spew; the guard catches the unwind.
    std::panic::set_hook(Box::new(|_| {}));
    let mut cursor = RollupCursor::new();
    let result = run_produce_guarded(&mut cursor, |_| panic!("boom"));
    let _ = std::panic::take_hook();
    assert_eq!(result.unwrap_err(), "sidebar produce panicked");
}

/// One forced cycle over a tempdir workspace, end to end and entirely in
/// process: the fast lane folds the published frame and posts a non-final
/// outcome, then the produce arm runs [`produce_snapshot`] on the same warm
/// cursor and posts the final reconciling outcome. Every forked enrichment is
/// pre-published fresh — the pane frame (the single-flight cache's fast path,
/// so no mux), the provider-spending stamp, and the accounts stamp — so the
/// cycle pays no subprocess and the test is hermetic.
#[test]
fn forced_cycle_posts_fast_then_inprocess_produce() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = workspace();
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    state.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();

    let now_ms = rimz::sidebar::snapshot::unix_now_ms();
    let frame = rimz::sidebar::snapshot::SnapshotCache {
        produced_at_ms: now_ms,
        session_name: "rimz-test".to_owned(),
        panes: vec![pane("terminal_7", "tab_1", false)],
    };
    std::fs::write(
        runtime.root.join("snapshot.json"),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.root.join("provider-spending.json"),
        now_ms,
        &rimz::agents::spending::Spending::default(),
        Default::default(),
    );
    let accounts = rimz::sidebar::snapshot::AccountsCache {
        refreshed_at_ms: now_ms,
        accounts: Default::default(),
        ok: true,
    };
    std::fs::write(
        runtime.root.join("accounts.json"),
        serde_json::to_vec(&accounts).unwrap(),
    )
    .unwrap();

    let config = ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 2,
        rimz_bin: PathBuf::from("rimz"),
    };
    let request = FetchRequest {
        force_produce: true,
        min_pane_cache_ms: None,
    };
    let mut cursor = RollupCursor::new();
    let mut outcomes = Vec::new();
    run_fetch_cycle(&config, &runtime, &state, request, &mut cursor, &mut |o| {
        outcomes.push(o)
    });

    assert_eq!(
        outcomes.len(),
        2,
        "fast paint, then the reconciling produce"
    );
    let fast = &outcomes[0];
    assert!(
        !fast.final_for_request,
        "the fast post leaves the cycle open for the produce"
    );
    let fast_snapshot = fast.snapshot.as_ref().expect("fast lane folds in process");
    assert!(
        !fast_snapshot.worktree_groups.is_empty(),
        "the fast fold renders the published pane"
    );
    let produced = &outcomes[1];
    assert!(produced.final_for_request, "the produce closes the cycle");
    let produced_snapshot = produced
        .snapshot
        .as_ref()
        .expect("the in-process produce succeeds on the published frame");
    assert!(
        !produced_snapshot.worktree_groups.is_empty(),
        "the produce folds the same pane frame"
    );
}

#[test]
fn producer_skips_produce_while_its_frame_is_within_one_tick() {
    // The two-speed contract: a ledger-delta storm paints per delta off the
    // in-process fast lane, producing at most once per data tick.
    assert!(!produce_this_cycle(true, false, Some(100), 1000));
    assert!(produce_this_cycle(true, false, Some(1000), 1000));
    assert!(
        produce_this_cycle(true, false, None, 1000),
        "no usable frame (cold start) always produces"
    );
}

#[test]
fn forced_requests_always_produce() {
    assert!(produce_this_cycle(true, true, Some(0), 1000));
    assert!(
        produce_this_cycle(false, true, Some(0), 1000),
        "a consumer reload/resize produces regardless of election"
    );
}

#[test]
fn consumer_never_produces_unforced_however_stale_the_frame() {
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
