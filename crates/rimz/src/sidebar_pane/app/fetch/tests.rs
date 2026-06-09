use super::*;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar::notify::{Notification, NotificationAgent, NotificationKind};
use crate::sidebar_pane::app::fixtures::{pane, workspace};
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

#[test]
fn refresh_override_stamps_folded_snapshot() {
    let workspace_id = workspace();
    let mut snapshot = super::super::state::placeholder_snapshot(workspace_id.clone());
    snapshot.sidebar.refresh_ms = 250;
    let config = ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 2,
        refresh_ms_override: Some(50),
        notification_prefs: NotificationsPrefs::default(),
        own_pane: None,
    };

    apply_refresh_override(&config, &mut snapshot);

    assert_eq!(snapshot.sidebar.refresh_ms, 50);
    assert_eq!(snapshot.sidebar.resolved_refresh_ms(), 50);
}

#[test]
fn notification_panes_keeps_live_agent_panes() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let notification = Notification {
        agents: vec![
            notification_agent("a1", Some(first.clone())),
            notification_agent("a2", None),
            notification_agent("a3", Some(second.clone())),
        ],
        notification_kind: NotificationKind::Coalesced,
        title: "Rimz: 3 agents need attention".to_owned(),
        body: "a1: waiting | a2: failed | a3: waiting".to_owned(),
    };

    assert_eq!(notification_panes(&notification), vec![first, second]);
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
        &runtime.shared_provider_spending_path(),
        now_ms,
        &crate::agents::spending::Spending::default(),
    );
    let accounts = crate::sidebar::snapshot::AccountsCache {
        refreshed_at_ms: now_ms,
        accounts: Default::default(),
        ok: true,
    };
    std::fs::write(
        runtime.shared_accounts_path(),
        serde_json::to_vec(&accounts).unwrap(),
    )
    .unwrap();

    let config = ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 2,
        refresh_ms_override: None,
        notification_prefs: NotificationsPrefs::default(),
        // No own pane: the fold must admit every published fixture pane even
        // when the test process itself runs inside a live mux pane.
        own_pane: None,
    };
    let request = FetchRequest {
        mode: FetchMode::HardRefresh,
        min_pane_cache_ms: None,
        published_frame_hint: false,
    };
    let mut cursor = RollupCursor::new();
    let mut notifications = NotificationState::default();
    let mut outcomes = Vec::new();
    run_fetch_cycle(
        FetchCycle {
            config: &config,
            runtime: &runtime,
            state: &state,
            notification_prefs: &NotificationsPrefs::default(),
            notifications: &mut notifications,
        },
        request,
        &mut cursor,
        &mut |o| outcomes.push(o),
    );

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

fn notification_agent(id: &str, pane_id: Option<PaneId>) -> NotificationAgent {
    NotificationAgent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from(id),
        label: format!("claude {id}"),
        pane_id,
    }
}

#[test]
fn cold_consumer_posts_frameless_rollup_while_waiting_for_first_publish() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = workspace();
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    state.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let mut rollup = SidebarSnapshot::build(
        workspace_id.clone(),
        Vec::new(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.display_name = "cold-room".to_owned();
    rollup.reflects_log = Some(crate::ledger::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    // A plain test-fixture write: the renderer's import graph stays free of
    // ledger writer APIs (`cargo xtask invariants`).
    std::fs::write(&state.latest_snapshot, serde_json::to_vec(&rollup).unwrap()).unwrap();

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
        refresh_ms_override: None,
        notification_prefs: NotificationsPrefs::default(),
        // No own pane: the fold must admit every published fixture pane even
        // when the test process itself runs inside a live mux pane.
        own_pane: None,
    };
    let mut cursor = RollupCursor::new();
    let mut notifications = NotificationState::default();
    let mut outcomes = Vec::new();
    run_fetch_cycle(
        FetchCycle {
            config: &config,
            runtime: &runtime,
            state: &state,
            notification_prefs: &NotificationsPrefs::default(),
            notifications: &mut notifications,
        },
        FetchRequest::default(),
        &mut cursor,
        &mut |outcome| outcomes.push(outcome),
    );

    assert_eq!(outcomes.len(), 1);
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.final_for_request);
    assert!(!outcome.fresh_pane_frame);
    let snapshot = outcome
        .snapshot
        .expect("waiting for the first pane frame is not a failed fetch");
    assert_eq!(snapshot.display_name, "cold-room");
    assert_eq!(snapshot.panes_produced_at_ms, None);
    assert!(
        snapshot.worktree_groups.is_empty(),
        "frameless folds carry rollup metadata but admit no pane cards"
    );
}

#[test]
fn consumer_miss_posts_the_rollup_error_as_the_final_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = workspace();
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    state.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    // A directory where the event log should be: the row scan's read fails,
    // and with no `latest.json` the rollup read has no fallback.
    std::fs::create_dir_all(&state.events_log).unwrap();

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
        refresh_ms_override: None,
        notification_prefs: NotificationsPrefs::default(),
        // No own pane: the fold must admit every published fixture pane even
        // when the test process itself runs inside a live mux pane.
        own_pane: None,
    };
    let mut cursor = RollupCursor::new();
    let mut notifications = NotificationState::default();
    let mut outcomes = Vec::new();
    run_fetch_cycle(
        FetchCycle {
            config: &config,
            runtime: &runtime,
            state: &state,
            notification_prefs: &NotificationsPrefs::default(),
            notifications: &mut notifications,
        },
        FetchRequest::default(),
        &mut cursor,
        &mut |outcome| outcomes.push(outcome),
    );

    assert_eq!(outcomes.len(), 1);
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.final_for_request);
    assert!(!outcome.fresh_pane_frame);
    let reason = outcome
        .snapshot
        .expect_err("an unreadable ledger rollup is the one failed consumer read");
    assert!(
        reason.contains(&state.events_log.display().to_string()),
        "the outcome names the unreadable path, got: {reason}"
    );
}

#[test]
fn fetch_request_sends_immediately_when_idle() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    let request = FetchRequest::producer_fresh_panes();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    assert_eq!(rx.try_recv().unwrap().mode, FetchMode::ProducerFreshPanes);
    assert!(dispatcher.pending_refetch.is_none());
}

#[test]
fn fetch_request_preserves_forced_pane_refresh_while_in_flight() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::default(), true);
    let request = FetchRequest::producer_fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    dispatcher.request(request, true);

    let pending = dispatcher.take_pending().expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::ProducerFreshPanes);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);
}

#[test]
fn hard_refresh_dominates_pending_producer_only_refresh() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::producer_fresh_panes(), true);
    let request = FetchRequest::hard_refresh();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    let pending = dispatcher.take_pending().expect("pending refetch");
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
        refresh_ms_override: None,
        notification_prefs: NotificationsPrefs::default(),
        // No own pane: the fold must admit every published fixture pane even
        // when the test process itself runs inside a live mux pane.
        own_pane: None,
    };
    let mut cursor = RollupCursor::new();
    let mut notifications = NotificationState::default();
    let mut outcomes = Vec::new();
    run_fetch_cycle(
        FetchCycle {
            config: &config,
            runtime: &runtime,
            state: &state,
            notification_prefs: &NotificationsPrefs::default(),
            notifications: &mut notifications,
        },
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
