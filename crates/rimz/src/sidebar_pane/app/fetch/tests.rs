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
    let (tx, rx) = result_channel();
    let mut sink = ResultSink::new(tx, PathBuf::from("/nonexistent/rimz-test.sock"), None);
    worker.run_cycle(state, request, &mut sink);
    drop(sink);
    rx.try_iter().map(|update| update.into_parts().0).collect()
}

fn snapshot(update: &FetchUpdate) -> &SidebarSnapshot {
    match update {
        FetchUpdate::Shared { update, .. } => snapshot(update),
        FetchUpdate::Snapshot { snapshot, .. } => snapshot,
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
    let (tx, rx) = result_channel();
    let mut sink = ResultSink::new(tx, PathBuf::from("missing.sock"), Some(50));

    sink.publish(FetchUpdate::Snapshot {
        snapshot: Box::new(folded),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
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
fn notification_reconciliation_reaches_the_younger_pane_before_its_bell() {
    use super::super::notify::{BellDecision, bell_decision};

    let fixture = FetchFixture::new();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let runtime = RuntimePaths::under(fixture.workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    UnreadEpisodes::default().persist(&runtime).unwrap();
    let mut worker = fixture.worker();
    worker.runtime = runtime.clone();
    worker.config.notification_prefs.coalesce_ms = 0;
    let subscribers = Subscribers::default();
    let (elder, elder_rx) = subscriber("01", "terminal_7", 2, "missing.sock".into());
    let (younger, younger_rx) = subscriber("02", "terminal_8", 2, "missing.sock".into());
    subscribe(&subscribers, elder);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers, Covered::default());
    let mut shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    shared.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    shared.worktree_groups[0].rows[0].pane = Some(pane("terminal_9", "tab_2", false));
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let frame = Arc::new(crate::sidebar::frame::assemble_frame(
        vec![
            pane("terminal_7", "tab_1", false),
            pane("terminal_8", "tab_2", false),
            pane("terminal_9", "tab_2", false),
        ],
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    ));
    let projected = worker.project_fold(
        WorkspaceFold {
            workspace: WorkspaceSnapshot(shared),
            frame: Some(frame),
        },
        &mut sink,
    );
    let wake_path = runtime.sidebar_socket_path(&fixture.instance_id);
    let notices = UnixDatagram::bind(&wake_path).unwrap();
    crate::wakeup::heartbeat::write_heartbeat(
        &runtime,
        fixture.workspace_id.clone(),
        &fixture.instance_id,
        MuxName::Zellij,
        "rimz-test",
        &wake_path,
        None,
        None,
    )
    .unwrap();
    notices
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    worker.publish_snapshot(
        &fixture.state,
        SnapshotPublication {
            snapshot: projected,
            phase: FetchPhase::Final,
            source: SnapshotSource::Cached,
        },
        &mut sink,
    );

    assert!(
        snapshot(&elder_rx.recv().unwrap())
            .rows()
            .any(|row| row.unread)
    );
    let younger = younger_rx.recv().unwrap();
    assert!(
        snapshot(&younger).rows().any(|row| row.unread),
        "this publication must carry the newly reconciled episode"
    );
    let mut bytes = [0; 4096];
    let len = notices.recv(&mut bytes).unwrap();
    let notice: crate::wakeup::events::SidebarEventEnvelope =
        serde_json::from_slice(&bytes[..len]).unwrap();
    let notice = notice.event;
    assert!(
        matches!(notice, SidebarEvent::Notify { ref panes, recheck_unread: true, .. } if panes.as_slice() == std::slice::from_ref(&target))
    );
    assert_eq!(
        bell_decision(snapshot(&younger), &[target], true),
        BellDecision::Fired
    );
}

#[test]
fn diagnostics_name_link_alerts() {
    let dir = tempfile::tempdir().unwrap();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace(), "rimz-test", None);
    emit_link_alert(
        &sink,
        LinkAlert {
            tier: crate::ids::LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 10,
            recovered_after_ms: None,
        },
    );

    let events = diagnostic_events(&sink);
    assert!(matches!(
        &events[0],
        crate::diag::record::DiagEvent::LinkAlert {
            tier: crate::ids::LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 10,
            recovered_after_ms: None,
        }
    ));
}

/// One forced cycle over a tempdir workspace, end to end and entirely in
/// process: the fast lane folds the published frame and posts a non-final
/// outcome, then the produce arm runs `produce_workspace_snapshot` on the same warm
/// cursor and posts the final reconciling outcome. Every forked enrichment is
/// pre-published fresh — the pane frame (the single-flight cache's fast path,
/// so no mux), the provider-spending stamp, and the accounts stamp — so the
/// cycle pays no subprocess and the test is hermetic.
#[test]
fn forced_cycle_retains_the_reconciling_produce_for_a_stalled_renderer() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = workspace();
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    state.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();

    let now_ms = crate::utils::time::unix_now_ms();
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
        &crate::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: now_ms,
            spending: crate::agents::spending::Spending::default(),
            ..Default::default()
        },
    );
    let accounts = crate::agents::account::AccountsCache {
        logins: crate::agents::known_kinds()
            .map(|kind| {
                (
                    crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(kind)),
                    crate::agents::account::ProviderRecord {
                        login: None,
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
    let request = FetchRequest {
        mode: FetchMode::HardRefresh,
        ..FetchRequest::default()
    };
    let mut worker = FetchWorker::new(config, runtime, crate::diag::DiagSink::disabled());
    let outcomes = run_cycle(&mut worker, &state, request);

    assert_eq!(
        outcomes.len(),
        1,
        "the reconciling produce replaces the unread fast snapshot"
    );
    let produced = &outcomes[0];
    assert!(produced.is_final(), "the produce closes the cycle");
    assert!(matches!(
        produced,
        FetchUpdate::Snapshot {
            source: SnapshotSource::Produced,
            ..
        }
    ));
    let produced_snapshot = snapshot(produced);
    assert!(
        !produced_snapshot.worktree_groups.is_empty(),
        "the produce folds the same pane frame"
    );
}

#[test]
fn produce_gate_bounds_normal_attempts_and_forces_refreshes() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    for (name, mode, frame_age_ms, expected) in [
        (
            "fresh producer frame skips produce",
            FetchMode::Normal,
            Some(100),
            false,
        ),
        (
            "stale producer frame produces",
            FetchMode::Normal,
            Some(1000),
            true,
        ),
        ("cold producer produces", FetchMode::Normal, None, true),
        (
            "producer-only freshness stays producer-only",
            FetchMode::FreshPanes,
            Some(0),
            true,
        ),
        (
            "producer hard refresh produces",
            FetchMode::HardRefresh,
            Some(0),
            true,
        ),
    ] {
        let mut cadence = ProducerCadence::default();
        assert_eq!(
            cadence.start_attempt_if_due(mode, frame_age_ms, tick, start),
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

    assert!(cadence.start_attempt_if_due(FetchMode::Normal, None, tick, start));
    assert!(
        !cadence.start_attempt_if_due(
            FetchMode::Normal,
            Some(10_000),
            tick,
            start + tick - Duration::from_nanos(1),
        ),
        "recording before the produce path throttles a failed cold attempt",
    );
    assert!(cadence.start_attempt_if_due(FetchMode::Normal, Some(10_000), tick, start + tick,));
}

#[test]
fn produce_gate_forced_attempts_bypass_and_advance_local_cadence() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    let mut cadence = ProducerCadence::default();

    assert!(cadence.start_attempt_if_due(FetchMode::Normal, None, tick, start));
    let forced_at = start + Duration::from_millis(10);
    assert!(cadence.start_attempt_if_due(FetchMode::FreshPanes, Some(0), tick, forced_at,));
    assert!(cadence.start_attempt_if_due(
        FetchMode::HardRefresh,
        Some(0),
        tick,
        forced_at + Duration::from_millis(10),
    ));
    assert!(
        !cadence.start_attempt_if_due(FetchMode::Normal, Some(10_000), tick, forced_at + tick,)
    );
}

#[test]
fn tab_name_memo_deduplicates_within_one_pane_observation() {
    let mut frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_7", "tab_1", false)],
        12,
        "rimz-test",
    );
    frame.topology_stamp_ms = Some(11);
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_7");
    let rename = crate::sidebar::produce::tab_status::TabRename {
        anchor: anchor.clone(),
        desired_name: "#feat ?".to_owned(),
        intent: crate::mux::tab_name::TabNameIntent::Status {
            observed: "#feat".to_owned(),
        },
    };
    let mut memo = TabNameMemo::default();

    assert_eq!(
        memo.pending(&frame, vec![rename.clone()]),
        vec![rename.clone()]
    );
    assert!(
        memo.pending(&frame, vec![rename.clone()]).is_empty(),
        "the same desired name is attempted once per pane observation",
    );

    let changed = crate::sidebar::produce::tab_status::TabRename {
        desired_name: "#feat !".to_owned(),
        ..rename.clone()
    };
    assert_eq!(
        memo.pending(&frame, vec![changed.clone()]),
        vec![changed.clone()],
        "a status change within the same observation still dispatches",
    );

    frame.observed_at_ms += 1;
    assert_eq!(
        memo.pending(&frame, vec![changed.clone()]),
        vec![changed],
        "a fresh pane observation permits a retry",
    );
}

#[test]
fn released_tab_deduplicates_and_settles_on_the_next_observation() {
    let mut pane = pane("terminal_7", "tab_1", false);
    pane.title = Some("opus".to_owned());
    pane.view_name = Some("opus".to_owned());
    let mut frame = crate::sidebar::frame::assemble_frame(vec![pane], 12, "rimz-test");
    frame.tabs[0].naming.owner = Some(crate::mux::tab_name::TabOwnerRecord {
        base: "opus".to_owned(),
        founders: Vec::new(),
    });
    let snapshot = SidebarSnapshot::build_with_agents(
        crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace"),
        Vec::new(),
        jiff::Timestamp::from_second(1_700_000_000).expect("time"),
    );
    let mut memo = TabNameMemo::default();
    let renames =
        crate::sidebar::produce::tab_status::desired_tab_renames(&snapshot, &frame, "zsh");
    assert!(matches!(
        renames[0].intent,
        crate::mux::tab_name::TabNameIntent::Release { .. }
    ));
    assert_eq!(memo.pending(&frame, renames.clone()).len(), 1);
    assert!(memo.pending(&frame, renames).is_empty());
    frame.observed_at_ms += 1;
    frame.tabs[0].name = Some("zsh".to_owned());
    let renames =
        crate::sidebar::produce::tab_status::desired_tab_renames(&snapshot, &frame, "zsh");
    assert!(memo.pending(&frame, renames).is_empty());
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

struct FetchFixture {
    _dir: tempfile::TempDir,
    workspace_id: WorkspaceId,
    state: StatePaths,
    runtime: RuntimePaths,
    instance_id: SidebarInstanceId,
}

#[test]
fn renderer_gets_the_snapshot_before_a_narrowing_probe() {
    thread_local! {
        static UPDATES: std::cell::RefCell<Option<ResultReceiver>> =
            const { std::cell::RefCell::new(None) };
        static SENT_BEFORE_PROBE: std::cell::Cell<Option<bool>> =
            const { std::cell::Cell::new(None) };
    }
    let fixture = FetchFixture::new();
    let mut worker = fixture.worker();
    let lost = [(AgentKind::new_unchecked("claude"), "lost".into())].into();
    crate::store::live_roster::publish(&fixture.state.live_roster, lost).unwrap();
    let (tx, rx) = result_channel();
    UPDATES.set(Some(rx));
    let mut sink = ResultSink::new(tx, PathBuf::from("missing.sock"), None);

    // The listing can take its whole timeout on a wedged server.
    worker.session_listed = |_, _| {
        let sent = UPDATES.with_borrow(|updates| {
            matches!(
                updates.as_ref().unwrap().try_recv(),
                Ok(FetchUpdate::Snapshot { .. })
            )
        });
        SENT_BEFORE_PROBE.set(Some(sent));
        false
    };
    worker.publish_snapshot(
        &fixture.state,
        SnapshotPublication {
            snapshot: SidebarSnapshot::build(
                fixture.workspace_id.clone(),
                Vec::new(),
                jiff::Timestamp::UNIX_EPOCH,
            ),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        },
        &mut sink,
    );

    assert_eq!(SENT_BEFORE_PROBE.get(), Some(true));
}

#[test]
fn roster_narrowing_needs_a_listed_session() {
    let fixture = FetchFixture::new();
    let diag = crate::diag::DiagSink::under(
        fixture._dir.path().to_path_buf(),
        fixture.workspace_id.clone(),
        "rimz-test",
        None,
    );
    let mut worker = fixture.worker();
    worker.diag = diag.clone();
    let roster = |ids: &[&str]| -> std::collections::BTreeSet<(AgentKind, AgentSessionId)> {
        ids.iter()
            .map(|id| (AgentKind::new_unchecked("claude"), (*id).into()))
            .collect()
    };
    let produced = |ids: &[&str]| {
        let now = jiff::Timestamp::now();
        let panes: Vec<_> = ids
            .iter()
            .map(|id| crate::sidebar::produce::test_support::pane(id, Some("claude"), None))
            .collect();
        let agents = ids
            .iter()
            .zip(&panes)
            .map(|(id, pane)| crate::agents::AgentState {
                status: crate::agents::AgentStatus::Running,
                pane: Some(pane.clone()),
                ..crate::testkit::agent_state("claude", id, now)
            })
            .collect();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(fixture.workspace_id.clone(), agents, now);
        snapshot.panes_produced_at_ms = Some(1);
        SnapshotPublication {
            snapshot: snapshot.with_live_panes(panes, None),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        }
    };
    let on_disk = || {
        crate::store::live_roster::read(&fixture.state.live_roster)
            .unwrap()
            .agents
    };
    let (tx, _rx) = result_channel();
    let mut sink = ResultSink::new(tx, PathBuf::from("missing.sock"), None);
    crate::store::live_roster::publish(&fixture.state.live_roster, roster(&["a", "b"])).unwrap();
    let before = std::fs::read(&fixture.state.live_roster).unwrap();

    // A producer that outlived its session sees no agents, every cycle.
    worker.session_listed = |_, _| false;
    worker.publish_snapshot(&fixture.state, produced(&[]), &mut sink);
    worker.publish_snapshot(&fixture.state, produced(&["a"]), &mut sink);
    worker.publish_snapshot(&fixture.state, produced(&[]), &mut sink);
    assert_eq!(std::fs::read(&fixture.state.live_roster).unwrap(), before);
    assert_eq!(on_disk(), roster(&["a", "b"]), "a birth still parks both");
    assert_eq!(
        diagnostic_events(&diag),
        [
            crate::diag::record::DiagEvent::LiveRosterHeld {
                dropped: roster(&["a", "b"]).into_iter().collect(),
            },
            crate::diag::record::DiagEvent::LiveRosterHeld {
                dropped: roster(&["b"]).into_iter().collect(),
            },
        ],
        "a repeat of one held set is rate-limited to one record",
    );

    // Nothing removed: written without asking the mux.
    worker.session_listed = |_, _| panic!("an additive publication must not probe");
    worker.publish_snapshot(&fixture.state, produced(&["a", "b", "c"]), &mut sink);
    assert_eq!(on_disk(), roster(&["a", "b", "c"]));

    // An agent that exits in a living room leaves the roster.
    worker.session_listed = |_, _| true;
    worker.publish_snapshot(&fixture.state, produced(&["a"]), &mut sink);
    assert_eq!(on_disk(), roster(&["a"]));
}

impl FetchFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = workspace();
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        state.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        Self {
            _dir: dir,
            workspace_id,
            state,
            runtime,
            instance_id: SidebarInstanceId::new(),
        }
    }

    fn worker(&self) -> FetchWorker {
        let config = test_config(self.workspace_id.clone(), self.instance_id.clone());
        let mut worker = FetchWorker::new(
            config,
            self.runtime.clone(),
            crate::diag::DiagSink::disabled(),
        );
        worker.producer_cadence.last_attempt = Some(Instant::now());
        worker.session_listed = |_, _| panic!("a unit test must not ask a multiplexer");
        worker
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
            crate::utils::time::unix_now_ms(),
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
}

#[test]
fn producer_fast_fold_publishes_workspace_content_without_a_pane_refresh() {
    let fixture = FetchFixture::new();
    fixture.write_pane_frame();
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
    assert!(matches!(first[0], FetchUpdate::Snapshot { .. }));
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
    assert!(matches!(second[0], FetchUpdate::Snapshot { .. }));
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
fn fast_fold_posts_frameless_rollup_before_first_publish() {
    let fixture = FetchFixture::new();
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
    let folded = snapshot(&outcome);
    assert_eq!(folded.display_name, "cold-room");
    assert_eq!(folded.panes_produced_at_ms, None);
    assert!(
        folded.worktree_groups.is_empty(),
        "frameless folds carry rollup metadata but admit no pane cards"
    );
}

#[test]
fn fast_fold_miss_posts_the_rollup_error_as_the_final_outcome() {
    let fixture = FetchFixture::new();
    std::fs::create_dir_all(&fixture.state.events_log).unwrap();
    let mut outcomes = fixture.run(FetchRequest::default());

    assert_eq!(outcomes.len(), 1);
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.is_final());
    let reason = match &outcome {
        FetchUpdate::Failed { error, .. } => error,
        _ => panic!("expected failed cache read"),
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
    let request = FetchRequest::fresh_panes();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    assert_eq!(rx.try_recv().unwrap().mode, FetchMode::FreshPanes);
    assert!(dispatcher.pending_refetch.is_none());

    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::default(), true);
    let request = FetchRequest::fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    dispatcher.request(request, true);

    rx.try_recv().expect("initial request");
    dispatcher.complete(true);
    let pending = rx.try_recv().expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::FreshPanes);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::fresh_panes(), true);
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
    dispatcher.defer_until(FetchRequest::fresh_panes(), earlier);

    assert_eq!(dispatcher.next_deadline(), Some(earlier));
    dispatcher.request(FetchRequest::default(), false);
    let request = rx
        .try_recv()
        .expect("immediate request absorbs deferred work");
    assert!(request.is_fresh_panes());
    assert!(dispatcher.next_deadline().is_none());
}

#[test]
fn fetch_dispatcher_fires_one_strongest_follow_up_after_in_flight_work() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("initial request");
    let due = Instant::now() + Duration::from_secs(3);
    dispatcher.defer_until(FetchRequest::fresh_panes(), due);

    dispatcher.fire_due(due);
    assert!(
        rx.try_recv().is_err(),
        "follow-up remains coalesced in flight"
    );
    dispatcher.complete(true);
    let follow_up = rx
        .try_recv()
        .expect("one follow-up dispatches on completion");
    assert!(follow_up.is_fresh_panes());
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
        FetchRequest::fresh_panes(),
        Instant::now() + Duration::from_secs(3),
    );

    dispatcher.complete(true);

    let follow_up = rx
        .try_recv()
        .expect("pending follow-up absorbs deferred work");
    assert!(follow_up.is_fresh_panes());
    assert!(dispatcher.next_deadline().is_none());
    assert!(rx.try_recv().is_err(), "only one follow-up dispatches");
}

#[test]
fn pane_frame_published_refolds_from_cache() {
    let fixture = FetchFixture::new();
    fixture.write_pane_frame();
    let mut outcomes = fixture.run(FetchRequest::pane_frame_published());

    assert_eq!(outcomes.len(), 1, "worker folds once from cache");
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.is_final());
    let snapshot = snapshot(&outcome);
    assert!(
        !snapshot.worktree_groups.is_empty(),
        "published panes are folded into the snapshot"
    );
}

#[test]
fn one_cycle_projects_the_shared_fold_once_for_each_renderer() {
    let fixture = FetchFixture::new();
    let panes = ["terminal_7", "terminal_8", "terminal_9"];
    let frame = crate::sidebar::frame::assemble_frame(
        panes.iter().map(|raw| pane(raw, "tab_1", false)).collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    std::fs::write(
        fixture.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();

    // Two panes of one host, eldest first; each excludes only itself.
    let subscribers = Subscribers::default();
    let mut updates = Vec::new();
    for (id, own) in [("01", "terminal_7"), ("02", "terminal_8")] {
        let (tx, rx) = result_channel();
        let instance_id = SidebarInstanceId::parse(&format!("sb_{id:0>32}")).unwrap();
        subscribers.lock().unwrap().insert(
            instance_id.as_str().to_owned(),
            Subscriber {
                instance_id,
                own_pane: Some(PaneId::from_parts(MuxName::Zellij, own)),
                tick_seconds: 2,
                refresh_override: None,
                tx,
                socket_path: PathBuf::from("missing.sock"),
            },
        );
        updates.push((own, rx));
    }
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, Covered::default());
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);

    for (own, rx) in updates {
        let received: Vec<FetchUpdate> = rx.try_iter().collect();
        assert_eq!(received.len(), 1, "one fold reaches the renderer of {own}");
        let shown: Vec<String> = snapshot(&received[0])
            .rows()
            .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.raw().to_owned()))
            .collect();
        let expected: Vec<&str> = panes.iter().copied().filter(|raw| *raw != own).collect();
        assert_eq!(
            shown, expected,
            "the renderer of {own} excludes only itself"
        );
    }
}

#[test]
fn a_stalled_subscriber_retains_only_the_latest_projected_fold() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (stalled, stalled_rx) = subscriber("01", "terminal_7", 60, "missing.sock".into());
    let (advancing, advancing_rx) = subscriber("02", "terminal_8", 60, "missing.sock".into());
    subscribe(&subscribers, stalled);
    subscribe(&subscribers, advancing);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, Covered::default());
    let now = crate::utils::time::unix_now_ms();
    for fold in 0..64 {
        tick_clock();
        let newest = format!("terminal_{}", 100 + fold);
        let frame = crate::sidebar::frame::assemble_frame(
            ["terminal_7", "terminal_8", &newest]
                .into_iter()
                .map(|id| pane(id, "tab_1", false))
                .collect(),
            now + fold,
            "rimz-test",
        );
        std::fs::write(
            fixture.runtime.pane_frame_path(),
            serde_json::to_vec(&frame).unwrap(),
        )
        .unwrap();
        worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
        let update = advancing_rx
            .try_recv()
            .expect("unrelated subscriber keeps advancing");
        assert_eq!(shown_panes(&update), ["terminal_7", &newest]);
        assert!(advancing_rx.try_recv().is_err());
    }
    let updates: Vec<_> = stalled_rx.try_iter().collect();
    assert_eq!(
        updates.len(),
        1,
        "a stalled consumer retains one snapshot, not one per fold"
    );
    assert_eq!(shown_panes(&updates[0]), ["terminal_8", "terminal_163"]);
    let (_, context) = updates.into_iter().next().unwrap().into_parts();
    assert!(
        Arc::ptr_eq(&context.unwrap(), sink.context.as_ref().unwrap()),
        "the newest snapshot keeps its own shared context"
    );
}

#[test]
fn shared_observation_extracts_common_once_and_keeps_each_own_view() {
    use crate::sidebar::observe;
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        (0..40)
            .flat_map(|n| {
                [
                    pane(&format!("terminal_{n}"), &format!("tab_{n}"), false),
                    pane(&format!("terminal_{}", n + 100), &format!("tab_{n}"), false),
                ]
            })
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    std::fs::write(
        fixture.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    let subscribers = Subscribers::default();
    let mut receivers = Vec::new();
    for n in 0..40 {
        let (sub, rx) = subscriber(
            &format!("{n:02}"),
            &format!("terminal_{n}"),
            2,
            PathBuf::from("missing.sock"),
        );
        receivers.push((sub.clone(), rx));
        subscribe(&subscribers, sub);
    }
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let mut worker = fixture.worker();
    worker.observer = Some(observe::RoomObserver::new(tx.clone()));
    let mut sink = ResultSink::shared(subscribers, Covered::default());
    observe::take_extractions();
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
    assert_eq!(
        observe::take_extractions(),
        (1, 0),
        "one common extraction for forty projections"
    );
    let mut common = None;
    for (n, (sub, rx)) in receivers.into_iter().enumerate() {
        let update = rx.try_recv().unwrap();
        let FetchUpdate::Shared { context, .. } = &update else {
            panic!("shared context missing")
        };
        let sig = context.observation.as_ref().unwrap();
        if let Some(first) = &common {
            assert!(Arc::ptr_eq(first, sig));
        } else {
            common = Some(sig.clone());
        }
        let mut config = worker.config.clone();
        config.instance_id = sub.instance_id;
        config.own_pane = sub.own_pane;
        let own = config.own_pane.clone().unwrap();
        let mut state = super::super::loop_state::LoopState::new(
            config,
            fixture.runtime.clone(),
            PathBuf::from("missing.sock"),
            crate::diag::DiagSink::disabled(),
            result_channel().1,
            None,
            tx.clone(),
            crate::sidebar_pane::pixel::PixelRenderCaps::default(),
            None,
        );
        state.apply_latest_snapshot(update);
        assert!(
            !state
                .current
                .rows()
                .any(|row| row.pane.as_ref().is_some_and(|pane| pane.pane_id == own))
        );
        assert_eq!(
            state.current.own_view.as_ref().unwrap().working_pane_ids,
            vec![PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", n + 100)
            )]
        );
    }
    assert_eq!(
        observe::take_extractions(),
        (0, 40),
        "only own parts are extracted per pane"
    );
}

#[test]
fn fold_file_reads_are_shared_across_forty_attachment_threads() {
    use crate::mux::focus_anchor;
    use crate::sidebar::{body_filter, read_marks};
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        (0..40)
            .flat_map(|n| {
                [
                    pane(&format!("terminal_{n}"), &format!("tab_{n}"), false),
                    pane(&format!("terminal_{}", n + 100), &format!("tab_{n}"), false),
                ]
            })
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    std::fs::write(
        fixture.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    let subscribers = Subscribers::default();
    let mut worker = fixture.worker();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut panes = Vec::new();
    for n in 0..40 {
        let (sub, rx) = subscriber(
            &format!("{n:02}"),
            &format!("terminal_{n}"),
            2,
            PathBuf::from("missing.sock"),
        );
        let mut config = worker.config.clone();
        config.instance_id = sub.instance_id.clone();
        config.own_pane = sub.own_pane.clone();
        subscribe(&subscribers, sub);
        let runtime = fixture.runtime.clone();
        let ready = ready_tx.clone();
        panes.push(std::thread::spawn(move || {
            let mut state = super::super::loop_state::LoopState::new(
                config,
                runtime,
                PathBuf::from("missing.sock"),
                crate::diag::DiagSink::disabled(),
                result_channel().1,
                None,
                std::sync::mpsc::sync_channel(64).0,
                crate::sidebar_pane::pixel::PixelRenderCaps::default(),
                None,
            );
            focus_anchor::take_reads();
            body_filter::take_reads();
            read_marks::take_store_reads();
            ready.send(()).unwrap();
            drop(ready);
            state.apply_latest_snapshot(rx.recv().unwrap());
            (
                focus_anchor::take_reads(),
                body_filter::take_reads(),
                read_marks::take_store_reads(),
            )
        }));
    }
    drop(ready_tx);
    for _ in 0..40 {
        ready_rx.recv().unwrap();
    }
    focus_anchor::take_reads();
    body_filter::take_reads();
    let mut sink = ResultSink::shared(subscribers, Covered::default());
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
    let mut reads = (focus_anchor::take_reads(), body_filter::take_reads(), 0);
    for pane in panes {
        let own = pane.join().unwrap();
        reads.0 += own.0;
        reads.1 += own.1;
        reads.2 += own.2;
    }
    assert_eq!(
        reads.0, 1,
        "focus-anchor observation is one room file read per fold"
    );
    assert_eq!(
        reads.1, 1,
        "body-filter observation is one room file read per fold"
    );
    assert_eq!(
        reads.2, 0,
        "attachments use the shared merged receipt baseline"
    );
}

fn subscriber(
    id: &str,
    own: &str,
    tick_seconds: u64,
    socket_path: PathBuf,
) -> (Subscriber, ResultReceiver) {
    let (tx, rx) = result_channel();
    let subscriber = Subscriber {
        instance_id: SidebarInstanceId::parse(&format!("sb_{id:0>32}")).unwrap(),
        own_pane: Some(PaneId::from_parts(MuxName::Zellij, own)),
        tick_seconds,
        refresh_override: None,
        tx,
        socket_path,
    };
    (subscriber, rx)
}

fn subscribe(subscribers: &Subscribers, subscriber: Subscriber) {
    subscribers
        .lock()
        .unwrap()
        .insert(subscriber.instance_id.as_str().to_owned(), subscriber);
}

fn unsubscribe(subscribers: &Subscribers, id: &str) {
    subscribers.lock().unwrap().remove(&format!("sb_{id:0>32}"));
}

fn shown_panes(update: &FetchUpdate) -> Vec<String> {
    snapshot(update)
        .rows()
        .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.raw().to_owned()))
        .collect()
}

/// Let the monotonic clock move past the last reading, so "observed before
/// the cycle started" is never a tie.
fn tick_clock() {
    std::thread::sleep(Duration::from_millis(2));
}

#[test]
fn staggered_requests_behind_one_fold_cost_at_most_two_folds() {
    const PANES: usize = 8;
    let fixture = FetchFixture::new();
    let panes: Vec<String> = (1..=PANES).map(|id| format!("terminal_{id}")).collect();
    let frame = crate::sidebar::frame::assemble_frame(
        panes.iter().map(|id| pane(id, "tab_1", false)).collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    std::fs::write(
        fixture.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    let subscribers = Subscribers::default();
    let mut receivers = Vec::new();
    for (index, own) in panes.iter().enumerate() {
        let (subscriber, rx) = subscriber(&(index + 1).to_string(), own, 60, "missing.sock".into());
        subscribe(&subscribers, subscriber);
        receivers.push(rx);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::force_fold()).unwrap();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = reads.clone();
    let receivers = Arc::new(receivers);
    let draining = receivers.clone();
    let first_publications = Arc::new(Mutex::new(Vec::new()));
    let captured = first_publications.clone();
    let sink = ResultSink::shared(subscribers, Covered::default());
    let mut worker = fixture.worker();
    let mut sender = Some(tx);
    let mut mid_first = None;
    worker.on_read = Some(Box::new(move || {
        let read = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        tick_clock();
        match read {
            1 => {
                mid_first = Some(Instant::now());
                for _ in 0..PANES {
                    sender
                        .as_ref()
                        .unwrap()
                        .send(FetchRequest::default())
                        .unwrap();
                }
            }
            2 => {
                *captured.lock().unwrap() =
                    draining.iter().map(|rx| rx.try_recv().unwrap()).collect();
                for _ in 0..PANES {
                    sender
                        .as_ref()
                        .unwrap()
                        .send(FetchRequest {
                            observed_at: mid_first.unwrap(),
                            ..FetchRequest::default()
                        })
                        .unwrap();
                }
                sender = None;
            }
            _ => panic!("a publication failed to cover the staggered burst"),
        }
    }));
    worker.run_resolving(rx, sink, |_| Ok(fixture.state.clone()));

    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "count only cycles crossing the store-reader boundary"
    );
    let first_publications = std::mem::take(&mut *first_publications.lock().unwrap());
    for ((own, rx), first) in panes.iter().zip(receivers.iter()).zip(first_publications) {
        let updates: Vec<_> = std::iter::once(first).chain(rx.try_iter()).collect();
        assert_eq!(
            updates.len(),
            2,
            "every subscriber receives both real publications"
        );
        for update in updates {
            assert_eq!(
                shown_panes(&update),
                panes
                    .iter()
                    .filter(|pane| *pane != own)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn requests_a_finished_fold_cannot_answer_still_run() {
    let (tx, rx) = std::sync::mpsc::channel();
    let covered = Covered::default();
    tx.send(FetchRequest::default()).unwrap();
    let mut sender = Some(tx);
    let mut cycles = Vec::new();
    let mut mid_first = None;
    drive(&rx, &covered, |cycle| {
        cycles.push(cycle.request);
        covered.record(cycle);
        let Some(tx) = sender.as_ref() else {
            return;
        };
        tick_clock();
        match cycles.len() {
            1 => {
                mid_first = Some(Instant::now());
                tx.send(FetchRequest::default()).unwrap();
            }
            // Observed before the second fold started, but asking more than
            // its ordinary request did.
            2 => {
                let observed_at = mid_first.unwrap();
                for request in [
                    FetchRequest::hard_refresh(),
                    FetchRequest::force_fold(),
                    FetchRequest::fresh_panes(),
                    FetchRequest::pane_frame_published(),
                ] {
                    tx.send(FetchRequest {
                        observed_at,
                        ..request
                    })
                    .unwrap();
                }
                sender = None;
            }
            _ => {}
        }
    });
    assert_eq!(cycles.len(), 3, "every stronger request runs");
    let last = cycles[2];
    assert_eq!(last.mode, FetchMode::HardRefresh);
    assert!(last.force_fold && last.published_frame_hint);
}

#[test]
fn a_fold_that_failed_answers_nothing() {
    let (tx, rx) = std::sync::mpsc::channel();
    let covered = Covered::default();
    tx.send(FetchRequest::default()).unwrap();
    let mut sender = Some(tx);
    let mut cycles = 0;
    let observed_at = Instant::now();
    tick_clock();
    // Nothing went out, so nothing is recorded.
    drive(&rx, &covered, |_cycle| {
        cycles += 1;
        if let Some(tx) = sender.take() {
            tx.send(FetchRequest {
                observed_at,
                ..FetchRequest::default()
            })
            .unwrap();
        }
    });
    assert_eq!(cycles, 2);
}

#[test]
fn a_follow_up_a_finished_fold_answered_is_not_sent() {
    let (tx, rx) = std::sync::mpsc::channel();
    let covered = Covered::default();
    let mut dispatcher = FetchDispatcher::for_plane(tx, covered.clone());
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("first request");
    dispatcher.request(FetchRequest::default(), true);
    tick_clock();
    covered.record(Coverage {
        started: Instant::now(),
        request: FetchRequest::default(),
    });

    dispatcher.complete(true);
    assert!(
        rx.try_recv().is_err(),
        "the plane's fold that started after it answers the follow-up"
    );
    assert!(
        !dispatcher.in_flight,
        "nothing is left waiting for an answer"
    );

    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("next request");
    dispatcher.request(FetchRequest::default(), true);
    dispatcher.complete(true);
    rx.try_recv()
        .expect("a follow-up observed after the last fold started goes out");
}

#[test]
fn one_publication_serves_the_renderers_its_cycle_began_with() {
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        ["terminal_7", "terminal_8", "terminal_9"]
            .iter()
            .map(|raw| pane(raw, "tab_1", false))
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    std::fs::write(
        fixture.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    let subscribers = Subscribers::default();
    let (eldest, eldest_rx) = subscriber("01", "terminal_7", 2, PathBuf::from("missing.sock"));
    let (younger, younger_rx) = subscriber("02", "terminal_8", 2, PathBuf::from("missing.sock"));
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers.clone(), Covered::default());
    sink.begin_cycle();
    let (workspace, frame) = worker.reader.read_workspace(&fixture.state).unwrap();
    let folded = worker.project_fold(WorkspaceFold { workspace, frame }, &mut sink);

    // The eldest leaves and a new pane arrives between fold and publication.
    unsubscribe(&subscribers, "01");
    let (newcomer, newcomer_rx) = subscriber("03", "terminal_9", 2, PathBuf::from("missing.sock"));
    subscribe(&subscribers, newcomer);
    sink.publish(FetchUpdate::Snapshot {
        snapshot: Box::new(folded),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    });

    let younger: Vec<_> = younger_rx.try_iter().collect();
    assert_eq!(younger.len(), 1);
    assert_eq!(
        shown_panes(&younger[0]),
        ["terminal_7", "terminal_9"],
        "the younger pane gets its own view, not the departed eldest's"
    );
    let eldest: Vec<_> = eldest_rx.try_iter().collect();
    assert_eq!(shown_panes(&eldest[0]), ["terminal_8", "terminal_9"]);
    assert!(
        newcomer_rx.try_iter().next().is_none(),
        "a pane that attached mid-cycle waits for its own first fetch"
    );
}

#[test]
fn the_worker_keeps_the_tick_of_its_eldest_renderer() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber("01", "terminal_7", 60, PathBuf::from("missing.sock"));
    let (younger, _younger_rx) = subscriber("02", "terminal_8", 1, PathBuf::from("missing.sock"));
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers.clone(), Covered::default());

    assert_eq!(worker.stand(&sink).tick, Duration::from_secs(60));
    unsubscribe(&subscribers, "01");
    sink.begin_cycle();
    assert_eq!(
        worker.stand(&sink).tick,
        Duration::from_secs(1),
        "the next eldest's tick takes over"
    );
}

#[test]
fn a_full_renderer_inbox_holds_neither_the_worker_nor_another_renderer() {
    let dir = tempfile::tempdir().unwrap();
    let full = dir.path().join("full.sock");
    let _stalled = std::os::unix::net::UnixDatagram::bind(&full).unwrap();
    crate::sidebar_pane::app::fixtures::fill_inbox(&full);
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber("01", "terminal_7", 2, dir.path().join("a.sock"));
    let (stalled, _stalled_rx) = subscriber("02", "terminal_8", 2, full);
    let (other, other_rx) = subscriber("03", "terminal_9", 2, dir.path().join("c.sock"));
    for subscriber in [eldest, stalled, other] {
        subscribe(&subscribers, subscriber);
    }
    let mut sink = ResultSink::shared(subscribers, Covered::default());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        sink.publish(FetchUpdate::Failed {
            error: "failed fold".into(),
        });
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the publication returns with one inbox full");
    assert!(matches!(
        other_rx.try_recv(),
        Ok(FetchUpdate::Failed { .. })
    ));
}
