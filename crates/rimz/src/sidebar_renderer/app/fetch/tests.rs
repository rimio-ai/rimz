use super::*;
use crate::sidebar_renderer::app::fixtures::{pane, workspace};
use crate::{MuxName, SidebarInstanceId};

#[test]
fn produce_guard_maps_an_error_to_a_degraded_outcome() {
    let mut cursor = RollupCursor::new();
    let result = run_produce_guarded(&mut cursor, |_| {
        Err(crate::sidebar::produce::ProduceErr::Fixture {
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

    let now_ms = crate::sidebar::snapshot::unix_now_ms();
    let frame = crate::sidebar::snapshot::assemble_frame(
        vec![pane("terminal_7", "tab_1", false)],
        now_ms,
        "rimz-test",
    );
    std::fs::write(
        runtime.root.join("snapshot.json"),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    crate::agents::spending::write_provider_spending_cache(
        &runtime.root.join("provider-spending.json"),
        now_ms,
        &crate::agents::spending::Spending::default(),
        Default::default(),
    );
    let accounts = crate::sidebar::snapshot::AccountsCache {
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
    };
    let request = FetchRequest {
        mode: FetchMode::HardRefresh,
        min_pane_cache_ms: None,
        published_frame_hint: false,
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
    assert!(!produce_this_cycle(
        true,
        FetchMode::Normal,
        Some(100),
        1000
    ));
    assert!(produce_this_cycle(
        true,
        FetchMode::Normal,
        Some(1000),
        1000
    ));
    assert!(
        produce_this_cycle(true, FetchMode::Normal, None, 1000),
        "no usable frame (cold start) always produces"
    );
}

#[test]
fn hard_refresh_requests_always_produce() {
    assert!(produce_this_cycle(
        true,
        FetchMode::HardRefresh,
        Some(0),
        1000
    ));
    assert!(
        produce_this_cycle(false, FetchMode::HardRefresh, Some(0), 1000),
        "a consumer reload/manual recovery produces regardless of election"
    );
}

#[test]
fn producer_only_fresh_panes_produces_only_on_the_producer() {
    assert!(produce_this_cycle(
        true,
        FetchMode::ProducerFreshPanes,
        Some(0),
        1000
    ));
    assert!(
        !produce_this_cycle(false, FetchMode::ProducerFreshPanes, Some(0), 1000),
        "consumers wait for the producer publication instead of local-producing"
    );
}

#[test]
fn consumer_never_produces_unforced_however_stale_the_frame() {
    // The storm-removal contract: staleness recovery belongs to the election
    // (the next-eldest becomes the producer within one heartbeat TTL), so a
    // wedged producer never turns every consumer into its own uncached
    // `list-panes` + git produce. The consumer keeps folding the held panes
    // with the event-fresh rollup — status stays live, only pane presence ages.
    assert!(!produce_this_cycle(
        false,
        FetchMode::Normal,
        Some(5_000),
        1000
    ));
    assert!(!produce_this_cycle(
        false,
        FetchMode::Normal,
        Some(60_000),
        1000
    ));
    assert!(
        !produce_this_cycle(false, FetchMode::Normal, None, 1000),
        "even a missing frame waits for the elected producer"
    );
}

#[test]
fn fetch_request_sends_immediately_when_idle() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut in_flight = false;
    let mut pending = None;
    let request = FetchRequest::producer_fresh_panes();

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    assert!(in_flight);
    assert_eq!(rx.try_recv().unwrap().mode, FetchMode::ProducerFreshPanes);
    assert!(pending.is_none());
}

#[test]
fn fetch_request_preserves_forced_pane_refresh_while_in_flight() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut in_flight = true;
    let mut pending = Some(FetchRequest::default());
    let request = FetchRequest::producer_fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    let pending = pending.expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::ProducerFreshPanes);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);
}

#[test]
fn hard_refresh_dominates_pending_producer_only_refresh() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut in_flight = true;
    let mut pending = Some(FetchRequest::producer_fresh_panes());
    let request = FetchRequest::hard_refresh();

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    assert!(in_flight);
    let pending = pending.expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::HardRefresh);
    assert!(pending.min_pane_cache_ms.is_some());
}

#[test]
fn pane_frame_published_request_is_read_only_but_marks_the_fold_fresh() {
    let request = FetchRequest::pane_frame_published();

    assert_eq!(request.mode, FetchMode::Normal);
    assert!(request.published_frame_hint);
}

#[test]
fn pane_frame_published_refolds_a_consumer_from_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = workspace();
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    state.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();

    let frame = crate::sidebar::snapshot::assemble_frame(
        vec![pane("terminal_7", "tab_1", false)],
        crate::sidebar::snapshot::unix_now_ms(),
        "rimz-test",
    );
    std::fs::write(
        runtime.root.join("snapshot.json"),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    let elder = SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").unwrap();
    let younger = SidebarInstanceId::parse("sb_019e8c565bbd7b22854f93a905e1034c").unwrap();
    crate::sidebar::write_heartbeat(
        &runtime,
        workspace_id.clone(),
        &elder,
        MuxName::Zellij,
        "rimz-test",
        &runtime.sock_dir.join("elder.sock"),
        None,
    )
    .unwrap();

    let config = ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: younger,
        tick_seconds: 2,
    };
    let mut cursor = RollupCursor::new();
    let mut outcomes = Vec::new();
    run_fetch_cycle(
        &config,
        &runtime,
        &state,
        FetchRequest::pane_frame_published(),
        &mut cursor,
        &mut |outcome| outcomes.push(outcome),
    );

    assert_eq!(outcomes.len(), 1, "consumer folds once from cache");
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.final_for_request);
    assert!(outcome.fresh_pane_frame);
    let snapshot = outcome.snapshot.expect("consumer fold succeeds");
    assert!(
        !snapshot.worktree_groups.is_empty(),
        "published panes are folded into the consumer snapshot"
    );
}
