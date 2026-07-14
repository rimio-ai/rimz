use super::*;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar::notify::{
    LinkAlert, LinkNotificationState, Notification, NotificationAgent, NotificationKind,
};
use crate::sidebar_pane::app::fixtures::{pane, workspace};
use crate::{MuxName, SidebarInstanceId, WorkspaceId};

#[test]
fn produce_guard_maps_failures_and_suppresses_renderer_panic_diagnostics() {
    let mut cursor = RollupCursor::new();
    let result = run_produce_guarded(&mut cursor, |_| {
        Err(crate::sidebar::produce::ProduceErr::Fixture {
            path: PathBuf::from("/nonexistent/panes.json"),
            reason: "injected failure".to_owned(),
        })
    });
    assert!(result.unwrap_err().contains("injected failure"));

    let _hook_guard = crate::sidebar_pane::app::PANIC_HOOK_TEST_LOCK
        .lock()
        .unwrap();
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_hook = observed.clone();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |_| {
        observed_hook.store(
            super::super::produce_panic_diagnostic_suppressed(),
            std::sync::atomic::Ordering::SeqCst,
        );
    }));

    let result = run_produce_guarded(&mut cursor, |_| panic!("boom"));
    std::panic::set_hook(previous_hook);

    assert_eq!(result.unwrap_err(), "sidebar produce panicked: boom");
    assert!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        "caught producer panics run under the diagnostic-suppression guard"
    );
}

#[test]
fn refresh_override_stamps_folded_snapshot() {
    let workspace_id = workspace();
    let mut snapshot = super::super::state::placeholder_snapshot(workspace_id.clone());
    snapshot.theme.display.refresh_ms = 250;
    let config = ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 2,
        refresh_ms_override: Some(50),
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: NotificationsPrefs::default(),
        nav_keys: crate::sidebar_pane::app::NavKeymap::from_config(
            &crate::config::SidebarKeys::default(),
        ),
        own_pane: None,
    };

    apply_refresh_override(&config, &mut snapshot);

    assert_eq!(snapshot.theme.display.refresh_ms, 50);
    assert_eq!(snapshot.theme.display.resolved_refresh_ms(), 50);
}

#[test]
fn notification_panes_target_agent_panes() {
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
        unread_count: None,
    };

    assert_eq!(notification_panes(&notification), vec![first, second]);
}

#[test]
fn diagnostics_name_producer_transitions_and_link_alerts() {
    let dir = tempfile::tempdir().unwrap();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace(), "rimz-test", None);
    let elder = SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").unwrap();
    let new_elder = SidebarInstanceId::parse("sb_019e8c565bbd7b22854f93a905e1034c").unwrap();

    let mut last = None;
    emit_producer_transition(
        &sink,
        &mut last,
        ProducerElection {
            elder: Some(elder.clone()),
        },
    );
    emit_producer_transition(&sink, &mut last, ProducerElection { elder: None });
    emit_producer_transition(
        &sink,
        &mut last,
        ProducerElection {
            elder: Some(new_elder.clone()),
        },
    );

    let events = diagnostic_events(&sink);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        crate::diag::record::DiagEvent::ProducerElected { prior_elder }
            if prior_elder == &elder
    ));
    assert!(matches!(
        &events[1],
        crate::diag::record::DiagEvent::ProducerDemoted { new_elder: observed }
            if observed == &new_elder
    ));

    emit_link_alert(
        &sink,
        LinkAlert {
            tier: crate::remote::link::LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 10,
            recovered_after_ms: None,
        },
    );

    let events = diagnostic_events(&sink);
    assert!(matches!(
        &events[2],
        crate::diag::record::DiagEvent::LinkAlert {
            tier: crate::remote::link::LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 10,
            recovered_after_ms: None,
        }
    ));
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

    let now_ms = crate::sidebar::timing::unix_now_ms();
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_7", "tab_1", false)],
        now_ms,
        "rimz-test",
    );
    std::fs::write(
        runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        now_ms,
        &crate::agents::spending::Spending::default(),
    );
    let accounts = crate::sidebar::refresh::AccountsCache {
        providers: crate::agents::known_kinds()
            .map(|kind| {
                (
                    kind.to_owned(),
                    crate::sidebar::refresh::ProviderRecord {
                        probed_at_ms: now_ms,
                        ok: true,
                        account: None,
                    },
                )
            })
            .collect(),
    };
    std::fs::write(
        runtime.shared_accounts_path(),
        serde_json::to_vec(&accounts).unwrap(),
    )
    .unwrap();

    let config = test_config(workspace_id, SidebarInstanceId::new());
    let election = ProducerElectionTracker::new(runtime.clone(), config.instance_id.clone());
    let request = FetchRequest {
        mode: FetchMode::HardRefresh,
        min_pane_cache_ms: None,
        published_frame_hint: false,
        force_fold: false,
    };
    let mut cursor = RollupCursor::new();
    let mut consumer_memo = ConsumerFoldMemo::default();
    let mut notifications = NotificationState::default();
    let mut link_notifications = LinkNotificationState::default();
    let mut outcomes = Vec::new();
    let mut last_election = None;
    run_fetch_cycle(
        FetchCycle {
            config: &config,
            runtime: &runtime,
            state: &state,
            notification_prefs: &NotificationsPrefs::default(),
            notifications: &mut notifications,
            link_notifications: &mut link_notifications,
            diag: &crate::diag::DiagSink::disabled(),
            election: &election,
            last_election: &mut last_election,
        },
        request,
        &mut cursor,
        &mut consumer_memo,
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
fn produce_gate_keeps_consumers_read_only_except_hard_refresh() {
    // The two-speed/storm-removal contract: the elected producer pays topology
    // produce at most once per data tick; consumers fold held panes unless the
    // user asks for a hard recovery refresh.
    for (name, is_producer, mode, frame_age_ms, expected) in [
        (
            "fresh producer frame skips produce",
            true,
            FetchMode::Normal,
            Some(100),
            false,
        ),
        (
            "stale producer frame produces",
            true,
            FetchMode::Normal,
            Some(1000),
            true,
        ),
        (
            "cold producer produces",
            true,
            FetchMode::Normal,
            None,
            true,
        ),
        (
            "producer-only freshness stays producer-only",
            true,
            FetchMode::ProducerFreshPanes,
            Some(0),
            true,
        ),
        (
            "consumer skips producer-only freshness",
            false,
            FetchMode::ProducerFreshPanes,
            Some(0),
            false,
        ),
        (
            "producer hard refresh produces",
            true,
            FetchMode::HardRefresh,
            Some(0),
            true,
        ),
        (
            "consumer hard refresh produces",
            false,
            FetchMode::HardRefresh,
            Some(0),
            true,
        ),
        (
            "stale consumer frame waits for election",
            false,
            FetchMode::Normal,
            Some(60_000),
            false,
        ),
        (
            "cold consumer waits for election",
            false,
            FetchMode::Normal,
            None,
            false,
        ),
    ] {
        assert_eq!(
            produce_this_cycle(is_producer, mode, frame_age_ms, 1000),
            expected,
            "{name}"
        );
    }
}

fn notification_agent(id: &str, pane_id: Option<PaneId>) -> NotificationAgent {
    NotificationAgent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from(id),
        label: format!("claude {id}"),
        handle: format!("claude {id}"),
        worktree: None,
        task: None,
        pane_id,
        root: None,
        ask_id: None,
        new_status: None,
    }
}

fn test_config(workspace_id: WorkspaceId, instance_id: SidebarInstanceId) -> ServeConfig {
    ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id,
        tick_seconds: 2,
        refresh_ms_override: None,
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: NotificationsPrefs::default(),
        nav_keys: crate::sidebar_pane::app::NavKeymap::from_config(
            &crate::config::SidebarKeys::default(),
        ),
        // No own pane: the fold must admit every published fixture pane even
        // when the test process itself runs inside a live mux pane.
        own_pane: None,
    }
}

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<crate::diag::record::DiagEvent> {
    std::fs::read_to_string(sink.log_path().unwrap())
        .expect("diagnostic log")
        .lines()
        .map(|line| {
            serde_json::from_str::<crate::diag::record::DiagEnvelope>(line)
                .expect("diagnostic envelope")
                .event
        })
        .collect()
}

struct ConsumerFixture {
    _dir: tempfile::TempDir,
    workspace_id: WorkspaceId,
    state: StatePaths,
    runtime: RuntimePaths,
    younger: SidebarInstanceId,
}

impl ConsumerFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = workspace();
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        state.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
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
        Self {
            _dir: dir,
            workspace_id,
            state,
            runtime,
            younger,
        }
    }

    fn run(&self, request: FetchRequest) -> Vec<FetchOutcome> {
        let mut cursor = RollupCursor::new();
        let mut consumer_memo = ConsumerFoldMemo::default();
        self.run_with(request, &mut cursor, &mut consumer_memo)
    }

    fn run_with(
        &self,
        request: FetchRequest,
        cursor: &mut RollupCursor,
        consumer_memo: &mut ConsumerFoldMemo,
    ) -> Vec<FetchOutcome> {
        let config = test_config(self.workspace_id.clone(), self.younger.clone());
        let election = ProducerElectionTracker::new(self.runtime.clone(), self.younger.clone());
        let mut notifications = NotificationState::default();
        let mut link_notifications = LinkNotificationState::default();
        let mut outcomes = Vec::new();
        let mut last_election = None;
        run_fetch_cycle(
            FetchCycle {
                config: &config,
                runtime: &self.runtime,
                state: &self.state,
                notification_prefs: &NotificationsPrefs::default(),
                notifications: &mut notifications,
                link_notifications: &mut link_notifications,
                diag: &crate::diag::DiagSink::disabled(),
                election: &election,
                last_election: &mut last_election,
            },
            request,
            cursor,
            consumer_memo,
            &mut |outcome| outcomes.push(outcome),
        );
        outcomes
    }

    fn write_pane_frame(&self) {
        let frame = crate::sidebar::frame::assemble_frame(
            vec![pane("terminal_7", "tab_1", false)],
            crate::sidebar::timing::unix_now_ms(),
            "rimz-test",
        );
        std::fs::write(
            self.runtime.pane_frame_path(),
            serde_json::to_vec(&frame).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn unchanged_consumer_inputs_skip_the_second_fold() {
    let fixture = ConsumerFixture::new();
    fixture.write_pane_frame();
    let mut rollup = SidebarSnapshot::build(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();

    let mut cursor = RollupCursor::new();
    let mut consumer_memo = ConsumerFoldMemo::default();
    let first = fixture.run_with(FetchRequest::default(), &mut cursor, &mut consumer_memo);
    assert_eq!(first.len(), 1);
    assert!(!first[0].unchanged);
    assert!(first[0].snapshot.is_ok());

    let second = fixture.run_with(FetchRequest::default(), &mut cursor, &mut consumer_memo);
    assert_eq!(second.len(), 1);
    assert!(second[0].unchanged);
    assert!(second[0].final_for_request);
}

#[test]
fn consumer_stamp_eligibility_excludes_every_mandatory_request() {
    assert!(consumer_stamp_eligible(FetchRequest::default(), false));
    assert!(!consumer_stamp_eligible(FetchRequest::default(), true));
    for request in [
        FetchRequest::force_fold(),
        FetchRequest::pane_frame_published(),
        FetchRequest::producer_fresh_panes(),
        FetchRequest::hard_refresh(),
        FetchRequest {
            min_pane_cache_ms: Some(1),
            ..FetchRequest::default()
        },
    ] {
        assert!(!consumer_stamp_eligible(request, false));
    }
}

#[test]
fn mandatory_consumer_fold_requires_one_ordinary_reseed_before_skip() {
    for mandatory in [
        FetchRequest::force_fold(),
        FetchRequest::pane_frame_published(),
        FetchRequest::producer_fresh_panes(),
    ] {
        let fixture = ConsumerFixture::new();
        fixture.write_pane_frame();
        let mut rollup = SidebarSnapshot::build(
            fixture.workspace_id.clone(),
            Vec::new(),
            jiff::Timestamp::now(),
        );
        rollup.reflects_log = Some(crate::store::event_log::LogExtent {
            generation: 0,
            offset: 0,
        });
        std::fs::write(
            &fixture.state.latest_snapshot,
            serde_json::to_vec(&rollup).unwrap(),
        )
        .unwrap();
        let mut cursor = RollupCursor::new();
        let mut memo = ConsumerFoldMemo::default();

        assert!(!fixture.run_with(FetchRequest::default(), &mut cursor, &mut memo)[0].unchanged);
        assert!(!fixture.run_with(mandatory, &mut cursor, &mut memo)[0].unchanged);
        assert!(
            !fixture.run_with(FetchRequest::default(), &mut cursor, &mut memo)[0].unchanged,
            "the first ordinary request reseeds the cleared memo",
        );
        assert!(
            fixture.run_with(FetchRequest::default(), &mut cursor, &mut memo)[0].unchanged,
            "the next unchanged request skips",
        );
    }
}

#[test]
fn force_fold_bypasses_consumer_unchanged_skip_without_fresh_pane_claim() {
    let fixture = ConsumerFixture::new();
    fixture.write_pane_frame();
    let mut rollup = SidebarSnapshot::build(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();
    let mut cursor = RollupCursor::new();
    let mut consumer_memo = ConsumerFoldMemo::default();
    assert!(
        !fixture.run_with(FetchRequest::default(), &mut cursor, &mut consumer_memo)[0].unchanged
    );

    let forced = fixture.run_with(FetchRequest::force_fold(), &mut cursor, &mut consumer_memo);

    assert_eq!(forced.len(), 1);
    assert!(!forced[0].unchanged);
    assert!(!forced[0].fresh_pane_frame);
    assert!(forced[0].snapshot.is_ok());
}

#[test]
fn cold_consumer_posts_frameless_rollup_while_waiting_for_first_publish() {
    let fixture = ConsumerFixture::new();
    let mut rollup = SidebarSnapshot::build(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.display_name = "cold-room".to_owned();
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();

    let mut outcomes = fixture.run(FetchRequest::default());

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
    let fixture = ConsumerFixture::new();
    std::fs::create_dir_all(&fixture.state.events_log).unwrap();
    let mut outcomes = fixture.run(FetchRequest::default());

    assert_eq!(outcomes.len(), 1);
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.final_for_request);
    assert!(!outcome.fresh_pane_frame);
    let reason = outcome
        .snapshot
        .expect_err("an unreadable store rollup is the one failed consumer read");
    assert!(
        reason.contains(&fixture.state.events_log.display().to_string()),
        "the outcome names the unreadable path, got: {reason}"
    );
}

#[test]
fn fetch_dispatcher_sends_idle_and_coalesces_strongest_pending_request() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    let request = FetchRequest::producer_fresh_panes();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    assert_eq!(rx.try_recv().unwrap().mode, FetchMode::ProducerFreshPanes);
    assert!(dispatcher.pending_refetch.is_none());

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

    let request = FetchRequest::pane_frame_published();
    assert_eq!(request.mode, FetchMode::Normal);
    assert!(request.published_frame_hint);
    assert!(!request.force_fold);

    let request = FetchRequest::force_fold();
    assert_eq!(request.mode, FetchMode::Normal);
    assert!(request.force_fold);
    assert!(!request.published_frame_hint);
    assert!(request.min_pane_cache_ms.is_none());
}

#[test]
fn pane_frame_published_refolds_a_consumer_from_cache() {
    let fixture = ConsumerFixture::new();
    fixture.write_pane_frame();
    let mut outcomes = fixture.run(FetchRequest::pane_frame_published());

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
