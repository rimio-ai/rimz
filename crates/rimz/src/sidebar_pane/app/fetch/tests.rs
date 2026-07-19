use super::*;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar::notify::{LinkAlert, Notification, NotificationAgent, NotificationKind};
use crate::sidebar_pane::app::fixtures::{pane, workspace};
use crate::{MuxName, SidebarInstanceId, WorkspaceId};

fn guarded_reader() -> (tempfile::TempDir, PublishedSnapshotReader) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(workspace(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (
        dir,
        PublishedSnapshotReader::new(runtime, "rimz-test", None),
    )
}

fn run_cycle(
    worker: &mut FetchWorker,
    state: &StatePaths,
    request: FetchRequest,
) -> Vec<FetchUpdate> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut sink = ResultSink::new(tx, PathBuf::from("/nonexistent/rimz-test.sock"), None);
    worker.run_cycle(state, request, &mut sink);
    drop(sink);
    rx.try_iter().collect()
}

fn snapshot(update: &FetchUpdate) -> &SidebarSnapshot {
    match update {
        FetchUpdate::Snapshot { snapshot, .. } => snapshot,
        FetchUpdate::Unchanged { .. } => panic!("expected snapshot, got unchanged"),
        FetchUpdate::Failed { error, .. } => panic!("expected snapshot, got: {error}"),
    }
}

#[test]
fn produce_guard_maps_failures_and_suppresses_renderer_panic_diagnostics() {
    let (_dir, mut reader) = guarded_reader();
    let result: std::result::Result<SidebarSnapshot, String> =
        run_produce_guarded(&mut reader, |_| {
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

    let result: std::result::Result<SidebarSnapshot, String> =
        run_produce_guarded(&mut reader, |_| panic!("boom"));
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
    let mut folded = SidebarSnapshot::build_with_agents(
        workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::UNIX_EPOCH,
    );
    folded.theme.display.refresh_ms = 250;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut sink = ResultSink::new(tx, PathBuf::from("missing.sock"), Some(50));

    sink.publish(FetchUpdate::Snapshot {
        snapshot: Box::new(folded),
        role: FetchRole::Consumer,
        phase: FetchPhase::Final,
        pane_frame: PaneFrame::Held,
    });

    let update = rx.recv().expect("published snapshot");
    let snapshot = snapshot(&update);
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
        title: "RimZ: 3 agents need attention".to_owned(),
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
    let mut worker = FetchWorker::new(
        config,
        runtime,
        NotificationsPrefs::default(),
        crate::diag::DiagSink::disabled(),
        election,
    );
    let outcomes = run_cycle(&mut worker, &state, request);

    assert_eq!(
        outcomes.len(),
        2,
        "fast paint, then the reconciling produce"
    );
    let fast = &outcomes[0];
    assert!(
        !fast.is_final(),
        "the fast post leaves the cycle open for the produce"
    );
    let fast_snapshot = snapshot(fast);
    assert!(
        !fast_snapshot.worktree_groups.is_empty(),
        "the fast fold renders the published pane"
    );
    let produced = &outcomes[1];
    assert!(produced.is_final(), "the produce closes the cycle");
    let produced_snapshot = snapshot(produced);
    assert!(
        !produced_snapshot.worktree_groups.is_empty(),
        "the produce folds the same pane frame"
    );
}

#[test]
fn produce_gate_bounds_normal_attempts_and_keeps_consumers_read_only() {
    // The two-speed/storm-removal contract: the elected producer pays topology
    // produce at most once per data tick; consumers fold held panes unless the
    // user asks for a hard recovery refresh.
    let tick = Duration::from_secs(1);
    let start = Instant::now();
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
        let mut cadence = ProducerCadence::default();
        assert_eq!(
            cadence.start_attempt_if_due(is_producer, mode, frame_age_ms, tick, start),
            expected,
            "{name}"
        );
    }
}

#[test]
fn produce_gate_throttles_cold_stale_and_failed_attempts_at_tick_boundary() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    let mut cadence = ProducerCadence::default();

    assert!(cadence.start_attempt_if_due(true, FetchMode::Normal, None, tick, start));
    assert!(
        !cadence.start_attempt_if_due(
            true,
            FetchMode::Normal,
            Some(10_000),
            tick,
            start + tick - Duration::from_nanos(1),
        ),
        "recording before the produce path throttles a failed cold attempt",
    );
    assert!(cadence.start_attempt_if_due(
        true,
        FetchMode::Normal,
        Some(10_000),
        tick,
        start + tick,
    ));
}

#[test]
fn produce_gate_forced_attempts_bypass_and_advance_local_cadence() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    let mut cadence = ProducerCadence::default();

    assert!(cadence.start_attempt_if_due(true, FetchMode::Normal, None, tick, start));
    let forced_at = start + Duration::from_millis(10);
    assert!(cadence.start_attempt_if_due(
        true,
        FetchMode::ProducerFreshPanes,
        Some(0),
        tick,
        forced_at,
    ));
    assert!(cadence.start_attempt_if_due(
        false,
        FetchMode::HardRefresh,
        Some(0),
        tick,
        forced_at + Duration::from_millis(10),
    ));
    assert!(!cadence.start_attempt_if_due(
        true,
        FetchMode::Normal,
        Some(10_000),
        tick,
        forced_at + tick,
    ));
}

#[test]
fn produce_gate_newly_promoted_consumer_starts_without_cadence_debt() {
    let tick = Duration::from_secs(1);
    let now = Instant::now();
    let mut cadence = ProducerCadence::default();

    assert!(!cadence.start_attempt_if_due(false, FetchMode::Normal, Some(10_000), tick, now,));
    assert!(cadence.start_attempt_if_due(true, FetchMode::Normal, Some(10_000), tick, now,));
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

    fn worker(&self) -> FetchWorker {
        let config = test_config(self.workspace_id.clone(), self.younger.clone());
        let election = ProducerElectionTracker::new(self.runtime.clone(), self.younger.clone());
        FetchWorker::new(
            config,
            self.runtime.clone(),
            NotificationsPrefs::default(),
            crate::diag::DiagSink::disabled(),
            election,
        )
    }

    fn run(&self, request: FetchRequest) -> Vec<FetchUpdate> {
        self.run_with(request, &mut self.worker())
    }

    fn run_with(&self, request: FetchRequest, worker: &mut FetchWorker) -> Vec<FetchUpdate> {
        run_cycle(worker, &self.state, request)
    }

    fn write_pane_frame(&self) {
        let mut frame = crate::sidebar::frame::assemble_frame(
            vec![pane("terminal_7", "tab_1", false)],
            crate::sidebar::timing::unix_now_ms(),
            "rimz-test",
        );
        frame.topology_stamp_ms = Some(11);
        frame.metrics_stamp_ms = Some(12);
        std::fs::write(
            self.runtime.pane_frame_path(),
            serde_json::to_vec(&frame).unwrap(),
        )
        .unwrap();
    }

    fn publish_projection(&self) {
        let snapshot: SidebarSnapshot =
            serde_json::from_slice(&std::fs::read(&self.state.latest_snapshot).unwrap()).unwrap();
        let frame = crate::sidebar::cache::read_snapshot_cache(
            &self.runtime.pane_frame_path(),
            "rimz-test",
        )
        .unwrap();
        crate::sidebar::workspace_projection::WorkspaceProjectionPublisher::default()
            .publish(
                &self.runtime,
                "rimz-test",
                &crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
                &frame,
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

    let mut worker = fixture.worker();
    let first = fixture.run_with(FetchRequest::default(), &mut worker);
    assert_eq!(first.len(), 1);
    assert!(matches!(first[0], FetchUpdate::Snapshot { .. }));

    let second = fixture.run_with(FetchRequest::default(), &mut worker);
    assert_eq!(second.len(), 1);
    assert!(matches!(second[0], FetchUpdate::Unchanged { .. }));
    assert!(second[0].is_final());
}

#[test]
fn producer_fast_fold_publishes_workspace_content_without_a_pane_refresh() {
    let fixture = ConsumerFixture::new();
    fixture.write_pane_frame();
    std::fs::remove_dir_all(&fixture.runtime.heartbeat_dir).unwrap();
    std::fs::create_dir_all(&fixture.runtime.heartbeat_dir).unwrap();
    let mut rollup = SidebarSnapshot::build(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.display_name = "first".to_owned();
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();
    let mut worker = fixture.worker();

    let first = fixture.run_with(FetchRequest::default(), &mut worker);
    assert!(matches!(
        first[0],
        FetchUpdate::Snapshot {
            role: FetchRole::Producer,
            ..
        }
    ));
    let published =
        crate::sidebar::workspace_projection::read_workspace_projection(&fixture.runtime)
            .expect("producer fast-fold projection");
    assert_eq!(published.projection.snapshot().display_name, "first");
    let source = published.source;

    rollup.display_name = "second-and-longer".to_owned();
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();
    let second = fixture.run_with(FetchRequest::default(), &mut worker);
    assert!(matches!(
        second[0],
        FetchUpdate::Snapshot {
            role: FetchRole::Producer,
            ..
        }
    ));
    let republished =
        crate::sidebar::workspace_projection::read_workspace_projection(&fixture.runtime)
            .expect("republished producer fast-fold projection");
    assert_eq!(republished.source, source);
    assert_eq!(
        republished.projection.snapshot().display_name,
        "second-and-longer",
    );
}

#[test]
fn adopted_consumer_uses_slim_stamp_until_truth_moves() {
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
    fixture.publish_projection();
    let mut worker = fixture.worker();

    assert!(matches!(
        fixture.run_with(FetchRequest::default(), &mut worker)[0],
        FetchUpdate::Snapshot { .. }
    ));
    assert!(worker.consumer_memo.last_was_adoption());

    std::fs::write(fixture.runtime.diff_stats_path(), b"producer-owned-change").unwrap();
    assert!(matches!(
        fixture.run_with(FetchRequest::default(), &mut worker)[0],
        FetchUpdate::Unchanged { .. }
    ));

    crate::store::event_log::append(
        &fixture.state.events_log,
        &crate::store::event::EventEnvelope::session_rebirth(
            fixture.workspace_id.clone(),
            "rimz-test",
        ),
    )
    .unwrap();
    assert!(matches!(
        fixture.run_with(FetchRequest::default(), &mut worker)[0],
        FetchUpdate::Snapshot { .. }
    ));
    assert!(
        !worker.consumer_memo.last_was_adoption(),
        "a stale projection falls back and restores the full stamp"
    );
}

#[test]
fn consumer_stamp_skip_and_record_eligibility_are_separate() {
    assert!(consumer_stamp_skippable(FetchRequest::default(), false));
    assert!(consumer_stamp_recordable(FetchRequest::default(), false));
    assert!(!consumer_stamp_skippable(FetchRequest::default(), true));
    assert!(!consumer_stamp_recordable(FetchRequest::default(), true));
    let publication = FetchRequest::pane_frame_published();
    assert!(!consumer_stamp_skippable(publication, false));
    assert!(consumer_stamp_recordable(publication, false));
    for request in [
        FetchRequest::force_fold(),
        FetchRequest::producer_fresh_panes(),
        FetchRequest::hard_refresh(),
        FetchRequest {
            min_pane_cache_ms: Some(1),
            ..FetchRequest::default()
        },
    ] {
        assert!(!consumer_stamp_skippable(request, false));
        assert!(!consumer_stamp_recordable(request, false));
    }
}

#[test]
fn consumer_stamp_pane_publication_seeds_next_ordinary_skip() {
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
    let mut worker = fixture.worker();

    assert!(matches!(
        fixture.run_with(FetchRequest::pane_frame_published(), &mut worker,)[0],
        FetchUpdate::Snapshot { .. }
    ),);
    assert!(
        matches!(
            fixture.run_with(FetchRequest::default(), &mut worker)[0],
            FetchUpdate::Unchanged { .. }
        ),
        "the publication fold seeds the ordinary-request memo",
    );
}

#[test]
fn consumer_stamp_other_mandatory_folds_clear_before_ordinary_reseed() {
    for mandatory in [
        FetchRequest::force_fold(),
        FetchRequest::producer_fresh_panes(),
        FetchRequest {
            mode: FetchMode::HardRefresh,
            min_pane_cache_ms: None,
            published_frame_hint: false,
            force_fold: false,
        },
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
        let mut worker = fixture.worker();

        assert!(matches!(
            fixture.run_with(FetchRequest::default(), &mut worker)[0],
            FetchUpdate::Snapshot { .. }
        ));
        assert!(matches!(
            fixture.run_with(mandatory, &mut worker)[0],
            FetchUpdate::Snapshot { .. }
        ));
        assert!(
            matches!(
                fixture.run_with(FetchRequest::default(), &mut worker)[0],
                FetchUpdate::Snapshot { .. }
            ),
            "the first ordinary request reseeds the cleared memo",
        );
        assert!(
            matches!(
                fixture.run_with(FetchRequest::default(), &mut worker)[0],
                FetchUpdate::Unchanged { .. }
            ),
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
    let mut worker = fixture.worker();
    assert!(matches!(
        fixture.run_with(FetchRequest::default(), &mut worker)[0],
        FetchUpdate::Snapshot { .. }
    ));

    let forced = fixture.run_with(FetchRequest::force_fold(), &mut worker);

    assert_eq!(forced.len(), 1);
    assert!(matches!(forced[0], FetchUpdate::Snapshot { .. }));
    assert_eq!(forced[0].pane_frame(), PaneFrame::Held);
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
    assert!(outcome.is_final());
    assert_eq!(outcome.pane_frame(), PaneFrame::Held);
    let folded = snapshot(&outcome);
    assert_eq!(folded.display_name, "cold-room");
    assert_eq!(folded.panes_produced_at_ms, None);
    assert!(
        folded.worktree_groups.is_empty(),
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
    assert!(outcome.is_final());
    assert_eq!(outcome.pane_frame(), PaneFrame::Held);
    let reason = match &outcome {
        FetchUpdate::Failed { error, .. } => error,
        _ => panic!("expected failed consumer read"),
    };
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

    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::default(), true);
    let request = FetchRequest::producer_fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    dispatcher.request(request, true);

    rx.try_recv().expect("initial request");
    dispatcher.complete(true);
    let pending = rx.try_recv().expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::ProducerFreshPanes);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::producer_fresh_panes(), true);
    let request = FetchRequest::hard_refresh();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    rx.try_recv().expect("initial request");
    dispatcher.complete(true);
    let pending = rx.try_recv().expect("pending refetch");
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
fn fetch_dispatcher_merges_deferred_deadlines_and_absorbs_work() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    let later = Instant::now() + Duration::from_secs(10);
    let earlier = later - Duration::from_secs(3);

    dispatcher.defer_until(FetchRequest::default(), later);
    dispatcher.defer_until(FetchRequest::producer_fresh_panes(), earlier);

    assert_eq!(dispatcher.next_deadline(), Some(earlier));
    dispatcher.request(FetchRequest::default(), false);
    let request = rx
        .try_recv()
        .expect("immediate request absorbs deferred work");
    assert!(request.is_producer_fresh_panes());
    assert!(dispatcher.next_deadline().is_none());
}

#[test]
fn fetch_dispatcher_fires_one_strongest_follow_up_after_in_flight_work() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("initial request");
    let due = Instant::now() + Duration::from_secs(3);
    dispatcher.defer_until(FetchRequest::producer_fresh_panes(), due);

    dispatcher.fire_due(due);
    assert!(
        rx.try_recv().is_err(),
        "follow-up remains coalesced in flight"
    );
    dispatcher.complete(true);
    let follow_up = rx
        .try_recv()
        .expect("one follow-up dispatches on completion");
    assert!(follow_up.is_producer_fresh_panes());
    assert!(rx.try_recv().is_err(), "only one follow-up dispatches");
}

#[test]
fn fetch_dispatcher_completion_absorbs_deferred_into_pending_follow_up() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("initial request");
    dispatcher.request(FetchRequest::default(), true);
    dispatcher.defer_until(
        FetchRequest::producer_fresh_panes(),
        Instant::now() + Duration::from_secs(3),
    );

    dispatcher.complete(true);

    let follow_up = rx
        .try_recv()
        .expect("pending follow-up absorbs deferred work");
    assert!(follow_up.is_producer_fresh_panes());
    assert!(dispatcher.next_deadline().is_none());
    assert!(rx.try_recv().is_err(), "only one follow-up dispatches");
}

#[test]
fn pane_frame_published_refolds_a_consumer_from_cache() {
    let fixture = ConsumerFixture::new();
    fixture.write_pane_frame();
    let mut outcomes = fixture.run(FetchRequest::pane_frame_published());

    assert_eq!(outcomes.len(), 1, "consumer folds once from cache");
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.is_final());
    assert_eq!(outcome.pane_frame(), PaneFrame::Fresh);
    let snapshot = snapshot(&outcome);
    assert!(
        !snapshot.worktree_groups.is_empty(),
        "published panes are folded into the consumer snapshot"
    );
}
