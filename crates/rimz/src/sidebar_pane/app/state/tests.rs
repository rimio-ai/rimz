use super::*;
use crate::agents::AgentStatus;
use crate::diag::record::{DiagEvent, GateRule};
use crate::sidebar::read_marks::ReadMarkStore;
use crate::sidebar_pane::app::ServeConfig;
use crate::sidebar_pane::app::fetch::{FetchPhase, FetchRole, FetchUpdate, PaneFrame};
use crate::sidebar_pane::app::fixtures::{snapshot, workspace};
use crate::sidebar_pane::app::health::ALERT_AFTER_FAILURES;
use crate::sidebar_pane::app::loop_state::LoopState;
use crate::sidebar_pane::pets::PixelRenderCaps;
use crate::sidebar_pane::render::{Alert, GateNotice};
use crate::{
    AgentCard, PaneId, RowCard, RuntimePaths, SidebarInstanceId, SidebarStatusCount,
    SidebarWorktreeGroup, WorkspaceId,
};

fn degraded_health(reason: &str) -> Health {
    Health {
        failure_streak: ALERT_AFTER_FAILURES,
        alert: Some(Alert::active(reason, jiff::Timestamp::now())),
    }
}

fn serve_config(ws: &WorkspaceId) -> ServeConfig {
    ServeConfig {
        workspace_id: ws.clone(),
        mux: crate::MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 1,
        refresh_ms_override: None,
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: crate::config::NotificationsPrefs::default(),
        nav_keys: crate::sidebar_pane::app::NavKeymap::from_config(
            &crate::config::SidebarKeys::default(),
        ),
        own_pane: None,
    }
}

fn fixed_time(second: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(second).expect("fixed timestamp")
}

fn fetch_failed(reason: &str, committed: &SidebarSnapshot, health: &Health) -> RenderState {
    compute_next_state(Err(reason.to_owned()), committed, health)
}

fn active_alert(health: &Health) -> &Alert {
    let alert = health.alert.as_ref().expect("active alert");
    assert!(alert.is_active());
    alert
}

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<DiagEvent> {
    let path = sink.log_path().unwrap();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => panic!("diagnostic log: {err}"),
    };
    text.lines()
        .map(|line| {
            serde_json::from_str::<crate::diag::record::DiagEnvelope>(line)
                .expect("diagnostic envelope")
                .event
        })
        .collect()
}

fn notification_trace_events(root: &std::path::Path) -> Vec<crate::diag::notify::NotifyTraceEvent> {
    let path = root.join("notify.log.jsonl");
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => panic!("notification trace log: {err}"),
    };
    text.lines()
        .map(|line| {
            serde_json::from_str::<crate::diag::notify::NotifyTraceEnvelope>(line)
                .expect("notification trace envelope")
                .event
        })
        .collect()
}

fn row_snapshot(ws: &WorkspaceId, status: AgentStatus, focused: bool) -> SidebarSnapshot {
    row_snapshot_at(ws, status, focused, fixed_time(1_700_000_000))
}

fn row_snapshot_at(
    ws: &WorkspaceId,
    status: AgentStatus,
    focused: bool,
    last_activity: jiff::Timestamp,
) -> SidebarSnapshot {
    let pane_id = PaneId::from_parts(crate::MuxName::Tmux, "%1");
    let mut snap = snapshot(ws);
    snap.worktree_groups = vec![SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: vec![SidebarStatusCount { status, count: 1 }],
        rows: vec![crate::SidebarRow {
            id: "sess-1".to_owned(),
            name: "claude".to_owned(),
            pane: Some(crate::pane::PaneRef::from_id(pane_id.clone())),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity,
            card: RowCard::Agent(Box::new(AgentCard {
                status,
                phase: crate::agents::TurnPhase::Idle,
                ..AgentCard::default()
            })),
        }],
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_number: None,
    }];
    if focused {
        snap.own_view = Some(crate::SidebarOwnView {
            sibling_count: 2,
            working_pane_ids: vec![pane_id.clone()],
            own_view_is_daemon: false,
        });
        snap.focused_pane = Some(pane_id.clone());
        snap.viewed_panes = vec![pane_id];
    }
    snap
}

fn snapshot_in_group(
    kind: crate::SidebarWorktreeKind,
    key: &str,
    pane: &str,
    cwd: Option<&str>,
) -> SidebarSnapshot {
    let mut pane_ref =
        crate::pane::PaneRef::from_id(PaneId::from_parts(crate::MuxName::Zellij, pane));
    pane_ref.cwd = cwd.map(ToOwned::to_owned);
    let row = crate::SidebarRow {
        id: pane.to_owned(),
        name: pane.to_owned(),
        pane: Some(pane_ref),
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: jiff::Timestamp::from_second(1_000).unwrap(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    };
    let group = SidebarWorktreeGroup {
        key: key.to_owned(),
        label: key.to_owned(),
        kind,
        status_counts: Vec::new(),
        rows: vec![row],
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_number: None,
    };
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        Vec::new(),
        jiff::Timestamp::from_second(1_000).unwrap(),
    );
    snapshot.worktree_groups = vec![group];
    snapshot
}

fn runtime_for(ws: &WorkspaceId) -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    (dir, runtime)
}

fn row_unread(snapshot: &SidebarSnapshot) -> bool {
    snapshot.worktree_groups[0].rows[0].unread
}

fn row_unread_by_id(snapshot: &SidebarSnapshot, row_id: &str) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .find(|row| row.id == row_id)
        .expect("row exists")
        .unread
}

fn row_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .map(|row| row.id.clone())
        .collect()
}

fn set_row_status(snapshot: &mut SidebarSnapshot, row_id: &str, status: AgentStatus) {
    let row = snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.id == row_id)
        .expect("row exists");
    row.as_agent_mut().expect("agent row").status = status;
}

fn append_agent_row(
    snapshot: &mut SidebarSnapshot,
    row_id: &str,
    raw_pane: &str,
    status: AgentStatus,
    unread: bool,
) {
    let mut row = snapshot.worktree_groups[0].rows[0].clone();
    row.id = row_id.to_owned();
    row.name = row_id.to_owned();
    row.pane = Some(crate::pane::PaneRef::from_id(PaneId::from_parts(
        crate::MuxName::Tmux,
        raw_pane,
    )));
    row.unread = unread;
    row.as_agent_mut().expect("agent row").status = status;
    snapshot.worktree_groups[0].rows.push(row);
}

fn set_all_rows_unread(snapshot: &mut SidebarSnapshot) {
    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        row.unread = true;
    }
}

fn set_viewed(snapshot: &mut SidebarSnapshot, viewed: bool) {
    if viewed {
        snapshot.viewed_panes = snapshot
            .focused_pane
            .iter()
            .cloned()
            .chain(
                snapshot
                    .own_view
                    .iter()
                    .flat_map(|view| view.working_pane_ids.iter().take(1).cloned()),
            )
            .take(1)
            .collect();
    } else {
        snapshot.viewed_panes.clear();
    }
}

fn force_tab_dwell_elapsed(harness: &mut ApplyHarness) {
    harness.tab_read_dwell_until =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
}

struct ApplyHarness {
    config: ServeConfig,
    state: LoopState,
}

impl ApplyHarness {
    fn new(ws: &WorkspaceId) -> (tempfile::TempDir, Self) {
        let (dir, runtime) = runtime_for(ws);
        (
            dir,
            Self::for_runtime(ws, runtime, SidebarInstanceId::new()),
        )
    }

    fn for_runtime(
        ws: &WorkspaceId,
        runtime: RuntimePaths,
        instance_id: SidebarInstanceId,
    ) -> Self {
        let mut config = serve_config(ws);
        config.instance_id = instance_id.clone();
        let (observe_tx, _observe_rx) = std::sync::mpsc::sync_channel(64);
        let mut state = LoopState::new(
            ws.clone(),
            Some(PaneId::from_parts(crate::MuxName::Tmux, "%sidebar")),
            None,
            observe_tx,
            ReadMarkStore::new(runtime, instance_id),
            PixelRenderCaps::default(),
            true,
        );
        state.current = snapshot(ws);
        Self { config, state }
    }

    fn apply(&mut self, snapshot: SidebarSnapshot) -> ApplyOutcome {
        self.apply_outcome(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            role: FetchRole::Producer,
            phase: FetchPhase::Final,
            pane_frame: PaneFrame::Fresh,
        })
    }

    fn apply_outcome(&mut self, outcome: FetchUpdate) -> ApplyOutcome {
        self.apply_outcome_with_diag(outcome, &crate::diag::DiagSink::disabled())
    }

    fn apply_outcome_with_diag(
        &mut self,
        outcome: FetchUpdate,
        diag: &crate::diag::DiagSink,
    ) -> ApplyOutcome {
        self.state
            .apply_fetch_outcome(&self.config, outcome, std::time::Instant::now(), diag)
    }

    fn fail(&mut self, reason: &str) -> ApplyOutcome {
        self.fail_with_diag(reason, &crate::diag::DiagSink::disabled())
    }

    fn fail_with_diag(&mut self, reason: &str, diag: &crate::diag::DiagSink) -> ApplyOutcome {
        self.apply_outcome_with_diag(
            FetchUpdate::Failed {
                error: reason.to_owned(),
                role: FetchRole::Producer,
                pane_frame: PaneFrame::Held,
            },
            diag,
        )
    }
}

impl std::ops::Deref for ApplyHarness {
    type Target = LoopState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl std::ops::DerefMut for ApplyHarness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

fn two_pane_snapshot(ws: &WorkspaceId, active: PaneId) -> (SidebarSnapshot, PaneId, PaneId) {
    let first = PaneId::from_parts(crate::MuxName::Tmux, "%1");
    let second = PaneId::from_parts(crate::MuxName::Tmux, "%2");
    let mut snap = row_snapshot(ws, crate::agents::AgentStatus::Running, false);
    let mut second_row = snap.worktree_groups[0].rows[0].clone();
    second_row.id = "sess-2".to_owned();
    second_row.name = "codex".to_owned();
    second_row.pane = Some(crate::pane::PaneRef::from_id(second.clone()));
    snap.worktree_groups[0].rows.push(second_row);
    snap.worktree_groups[0].status_counts[0].count = 2;
    snap.own_view = Some(crate::SidebarOwnView {
        sibling_count: 2,
        working_pane_ids: vec![first.clone(), second.clone()],
        own_view_is_daemon: false,
    });
    snap.focused_pane = Some(active.clone());
    snap.viewed_panes = vec![active];
    (snap, first, second)
}

#[test]
fn cwd_flap_within_one_group_is_not_a_migration() {
    use crate::SidebarWorktreeKind::External;
    // The pane stays in the `external` group while its cwd flaps between two
    // out-of-project paths — a cwd change, not a row moving between groups.
    let prev = snapshot_in_group(External, "external", "terminal_1", Some("/tmp/a"));
    let next = snapshot_in_group(External, "external", "terminal_1", Some("/tmp/b"));

    assert!(diff_group_migrations(&prev, &next).is_empty());
}

#[test]
fn newborn_worktree_settle_with_stable_cwd_is_not_a_migration() {
    use crate::SidebarWorktreeKind::{External, Worktree};
    // The pane is born in `external` before the worktree backing resolves, then
    // reclassifies to `worktree` while its cwd was the worktree dir all along.
    let prev = snapshot_in_group(External, "external", "terminal_1", Some("/repo/feature"));
    let next = snapshot_in_group(
        Worktree,
        "/repo/feature",
        "terminal_1",
        Some("/repo/feature"),
    );

    assert!(diff_group_migrations(&prev, &next).is_empty());
}

#[test]
fn moving_between_groups_records_one_migration() {
    let prev = snapshot_in_group(
        crate::SidebarWorktreeKind::External,
        "external",
        "terminal_1",
        Some("/tmp/a"),
    );
    let next = snapshot_in_group(
        crate::SidebarWorktreeKind::Worktree,
        "/repo/feature",
        "terminal_1",
        Some("/repo/feature"),
    );

    assert!(matches!(
        diff_group_migrations(&prev, &next).as_slice(),
        [DiagEvent::GroupMigration { pane_id, .. }] if pane_id.raw() == "terminal_1"
    ));
}

#[test]
fn diagnostics_scope_group_migrations_to_elder_only() {
    let ws = workspace();
    let prev = snapshot_in_group(
        crate::SidebarWorktreeKind::External,
        "external",
        "terminal_1",
        Some("/tmp/a"),
    );
    let next = snapshot_in_group(
        crate::SidebarWorktreeKind::Worktree,
        "/repo/feature",
        "terminal_1",
        Some("/repo/feature"),
    );
    let held_gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(fixed_time(1_700_000_000)),
        spend_carry_since: None,
        rule: Some(GateRule::EmptyStampedFrame),
    };
    let active_health = Health {
        failure_streak: ALERT_AFTER_FAILURES,
        alert: Some(Alert::active("snapshot failed", fixed_time(1_700_000_000))),
    };

    let consumer_dir = tempfile::tempdir().unwrap();
    let consumer_sink = crate::diag::DiagSink::under(
        consumer_dir.path().to_path_buf(),
        ws.clone(),
        "rimz-test",
        None,
    );
    emit_diagnostics(
        &consumer_sink,
        FetchDiagnostics {
            prev_snapshot: &prev,
            incoming_panes_produced_at_ms: next.panes_produced_at_ms,
            next_snapshot: &next,
            prev_health: &Health::default(),
            next_health: &active_health,
            prev_gate: &GateState::default(),
            next_gate: &held_gate,
            fetch_failure: Some("pane discovery failed".to_owned()),
            rejected: true,
            released_via_escape_hatch: false,
            is_elder: false,
            now: fixed_time(1_700_000_000),
        },
    );
    let consumer_events = diagnostic_events(&consumer_sink);

    assert!(
        consumer_events
            .iter()
            .any(|event| matches!(event, DiagEvent::FetchFailure { .. }))
    );
    assert!(
        consumer_events
            .iter()
            .any(|event| matches!(event, DiagEvent::GateHold { .. }))
    );
    assert!(
        consumer_events
            .iter()
            .any(|event| matches!(event, DiagEvent::HealthAlert { .. }))
    );
    assert!(
        !consumer_events
            .iter()
            .any(|event| matches!(event, DiagEvent::GroupMigration { .. }))
    );

    let elder_dir = tempfile::tempdir().unwrap();
    let elder_sink =
        crate::diag::DiagSink::under(elder_dir.path().to_path_buf(), ws, "rimz-test", None);
    emit_diagnostics(
        &elder_sink,
        FetchDiagnostics {
            prev_snapshot: &prev,
            incoming_panes_produced_at_ms: next.panes_produced_at_ms,
            next_snapshot: &next,
            prev_health: &Health::default(),
            next_health: &Health::default(),
            prev_gate: &GateState::default(),
            next_gate: &GateState::default(),
            fetch_failure: None,
            rejected: false,
            released_via_escape_hatch: false,
            is_elder: true,
            now: fixed_time(1_700_000_000),
        },
    );

    assert!(
        diagnostic_events(&elder_sink)
            .iter()
            .any(|event| matches!(event, DiagEvent::GroupMigration { .. }))
    );
}

#[test]
fn compute_next_state_keeps_frame_and_tracks_refresh_health() {
    let ws = workspace();
    let committed = snapshot(&ws);
    let ok = compute_next_state(Ok(snapshot(&ws)), &committed, &Health::default());
    assert!(ok.health.alert.is_none());
    assert_eq!(ok.health.failure_streak, 0);
    assert_eq!(ok.snapshot.workspace_id, ws);

    let previous = snapshot(&ws);
    let first_failure = fetch_failed("store not found", &previous, &Health::default());
    assert!(first_failure.health.alert.is_none());
    assert_eq!(first_failure.health.failure_streak, 1);
    assert_eq!(first_failure.snapshot.workspace_id, previous.workspace_id);

    let second_failure = fetch_failed(
        "store not found",
        &first_failure.snapshot,
        &first_failure.health,
    );
    let alert = active_alert(&second_failure.health);
    assert!(alert.reason.contains("snapshot failed"));
    assert!(alert.reason.contains("store not found"));

    let armed = degraded_health("snapshot failed: first");
    let first_since = armed.alert.as_ref().unwrap().since;
    let still_degraded = fetch_failed("second", &previous, &armed);
    let alert = still_degraded.health.alert.expect("still degraded");
    assert_eq!(alert.since, first_since, "since must remain pinned");
    assert!(alert.reason.contains("second"));

    let recovered = compute_next_state(Ok(snapshot(&ws)), &previous, &armed);
    let alert = recovered.health.alert.expect("recovered alert lingers");
    assert!(!alert.is_active());
    assert!(alert.recovered_at.is_some());
    assert_eq!(recovered.health.failure_streak, 0);
}

#[test]
fn accepted_final_then_failed_final_keeps_rendered_facts() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    let mut accepted = row_snapshot(&ws, AgentStatus::Waiting, false);
    accepted.worktree_groups[0].rows[0].unread = true;
    append_agent_row(&mut accepted, "sess-2", "%2", AgentStatus::Running, false);

    h.apply(accepted);
    let rows = row_ids(&h.current);
    let unread = h.current.rows().map(|row| row.unread).collect::<Vec<_>>();
    let statuses = h.current.rows().map(|row| row.status()).collect::<Vec<_>>();
    let gate_notice = h.ui.gate_notice.clone();

    h.fail("store not found");

    assert_eq!(row_ids(&h.current), rows);
    assert_eq!(
        h.current.rows().map(|row| row.unread).collect::<Vec<_>>(),
        unread
    );
    assert_eq!(
        h.current.rows().map(|row| row.status()).collect::<Vec<_>>(),
        statuses
    );
    assert_eq!(h.health.failure_streak, 1);
    assert_eq!(h.ui.gate_notice, gate_notice);
}

#[test]
fn interim_success_does_not_recover_health_and_final_failure_advances_once() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    h.health = degraded_health("snapshot failed: first");

    h.apply_outcome(FetchUpdate::Snapshot {
        snapshot: Box::new(row_snapshot(&ws, AgentStatus::Running, false)),
        role: FetchRole::Producer,
        phase: FetchPhase::Interim,
        pane_frame: PaneFrame::Held,
    });
    assert_eq!(h.health.failure_streak, ALERT_AFTER_FAILURES);
    assert!(h.health.alert.as_ref().is_some_and(Alert::is_active));

    h.fail("produce failed");

    assert_eq!(h.health.failure_streak, ALERT_AFTER_FAILURES + 1);
    let alert = active_alert(&h.health);
    assert!(alert.reason.contains("produce failed"));
}

#[test]
fn focused_read_clear_survives_failure_without_duplicate_trace() {
    let ws = workspace();
    let (dir, runtime) = runtime_for(&ws);
    let instance_id = SidebarInstanceId::new();
    let mut h = ApplyHarness::for_runtime(&ws, runtime, instance_id.clone());
    let diag = crate::diag::DiagSink::under(
        dir.path().to_path_buf(),
        ws.clone(),
        "rimz-test",
        Some(instance_id),
    );
    let mut focused = row_snapshot(&ws, AgentStatus::Waiting, true);
    focused.worktree_groups[0].rows[0].unread = true;

    h.apply_outcome_with_diag(
        FetchUpdate::Snapshot {
            snapshot: Box::new(focused),
            role: FetchRole::Producer,
            phase: FetchPhase::Final,
            pane_frame: PaneFrame::Fresh,
        },
        &diag,
    );
    assert!(!row_unread(&h.current));
    assert_eq!(
        notification_trace_events(dir.path())
            .iter()
            .filter(|event| matches!(
                event,
                crate::diag::notify::NotifyTraceEvent::UnreadCleared { .. }
            ))
            .count(),
        1
    );

    h.fail_with_diag("store not found", &diag);

    assert!(!row_unread(&h.current));
    assert_eq!(
        notification_trace_events(dir.path())
            .iter()
            .filter(|event| matches!(
                event,
                crate::diag::notify::NotifyTraceEvent::UnreadCleared { .. }
            ))
            .count(),
        1
    );
}

#[test]
fn cold_start_failure_uses_snapshot_builder_defaults() {
    let ws = workspace();
    let committed =
        SidebarSnapshot::build_with_agents(ws.clone(), Vec::new(), Timestamp::UNIX_EPOCH);
    let failed = fetch_failed("store not found", &committed, &Health::default());
    let expected = SidebarSnapshot::build_with_agents(ws, Vec::new(), failed.snapshot.now);

    assert_eq!(
        serde_json::to_value(&failed.snapshot).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn focus_writes_read_receipt_and_clears_current_unread_row() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime.clone(), instance_a.clone());
    let mut focused = row_snapshot_at(&ws, AgentStatus::Success, true, fixed_time(1_700_000_000));
    focused.worktree_groups[0].rows[0].unread = true;

    a.apply(focused);
    assert!(
        runtime.sidebar_read_marks_path(&instance_a).exists(),
        "a focused unread row writes a read receipt"
    );
    assert!(!row_unread(&a.current));
}

#[test]
fn focused_read_clear_keeps_live_rank_when_unread_clears() {
    let ws = workspace();
    let (_dir, mut a) = ApplyHarness::new(&ws);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_row_status(&mut background, "sess-1", AgentStatus::Success);
    set_row_status(&mut background, "sess-2", AgentStatus::Waiting);
    background.worktree_groups[0].rows[0].unread = true;
    set_viewed(&mut background, false);

    a.apply(background);
    assert_eq!(row_ids(&a.current), vec!["sess-2", "sess-1"]);

    let (mut viewed, _, _) = two_pane_snapshot(&ws, first.clone());
    set_row_status(&mut viewed, "sess-1", AgentStatus::Success);
    set_row_status(&mut viewed, "sess-2", AgentStatus::Waiting);
    viewed.worktree_groups[0].rows[0].unread = true;
    a.apply(viewed.clone());

    assert_eq!(
        row_ids(&a.current),
        vec!["sess-2", "sess-1"],
        "clearing unread keeps the focused row in live time/status rank"
    );
    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert_eq!(
        a.ui.selected_index, 1,
        "selection re-anchors to the focused row's ranked position"
    );

    let mut settled = viewed;
    settled.worktree_groups[0].rows[0].unread = false;
    a.ui.order_hold.as_mut().expect("hold armed").expires_ms =
        jiff::Timestamp::now().as_millisecond() - 1;
    a.apply(settled);

    assert_eq!(
        row_ids(&a.current),
        vec!["sess-2", "sess-1"],
        "after the hold expires the live rank stays unchanged"
    );
    assert_eq!(a.ui.selected_index, 1, "selection follows the same pane");
}

#[test]
fn focus_clears_sticky_unread_after_status_returns_to_running() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime.clone(), instance_a.clone());
    let mut focused = row_snapshot_at(&ws, AgentStatus::Running, true, fixed_time(1_700_000_100));
    focused.worktree_groups[0].rows[0].unread = true;

    a.apply(focused);

    assert!(
        runtime.sidebar_read_marks_path(&instance_a).exists(),
        "viewing a sticky unread row writes a read receipt even after status recovery"
    );
    assert!(!row_unread(&a.current));
}

#[test]
fn background_register_pane_does_not_focus_clear_until_viewed() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime.clone(), instance_a.clone());
    let read_marks = runtime.sidebar_read_marks_path(&instance_a);
    let mut background =
        row_snapshot_at(&ws, AgentStatus::Waiting, true, fixed_time(1_700_000_000));
    background.worktree_groups[0].rows[0].unread = true;
    background.viewed_panes.clear();

    a.apply(background);

    assert!(row_unread(&a.current));
    assert!(
        !read_marks.exists(),
        "a background register pane must not write a focus receipt until viewed"
    );

    let mut viewed = row_snapshot_at(&ws, AgentStatus::Waiting, true, fixed_time(1_700_000_000));
    viewed.worktree_groups[0].rows[0].unread = true;
    a.apply(viewed);

    assert!(
        read_marks.exists(),
        "viewing the register pane writes the normal focus receipt"
    );
    assert!(!row_unread(&a.current));
}

#[test]
fn tab_switch_in_arms_sibling_read_dwell() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime, instance_a);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_all_rows_unread(&mut background);
    set_viewed(&mut background, false);

    a.apply(background);

    assert!(row_unread_by_id(&a.current, "sess-1"));
    assert!(row_unread_by_id(&a.current, "sess-2"));
    assert_eq!(a.ui.viewing_own_tab, Some(false));

    let (mut viewed, _, _) = two_pane_snapshot(&ws, first);
    set_all_rows_unread(&mut viewed);
    a.apply(viewed);

    assert!(
        a.tab_read_dwell_until.is_some(),
        "switching into the tab arms the sibling dwell"
    );
    assert!(
        !row_unread_by_id(&a.current, "sess-1"),
        "the focused row reads immediately"
    );
    assert!(
        row_unread_by_id(&a.current, "sess-2"),
        "the sibling waits for dwell"
    );
    let marks = a.read_marks.load_merged();
    assert!(marks.cleared_at_ms("sess-1").is_some());
    assert!(marks.cleared_at_ms("sess-2").is_none());
}

#[test]
fn tab_dwell_elapsed_while_viewing_sweeps_siblings() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime, instance_a);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_all_rows_unread(&mut background);
    set_viewed(&mut background, false);
    a.apply(background);

    let (mut viewed, _, _) = two_pane_snapshot(&ws, first);
    set_all_rows_unread(&mut viewed);
    a.apply(viewed.clone());

    force_tab_dwell_elapsed(&mut a);
    a.apply(viewed);

    assert_eq!(a.tab_read_dwell_until, None);
    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert!(!row_unread_by_id(&a.current, "sess-2"));
    let marks = a.read_marks.load_merged();
    assert!(marks.cleared_at_ms("sess-1").is_some());
    assert!(marks.cleared_at_ms("sess-2").is_some());
}

#[test]
fn tab_switch_clear_keeps_live_rank_when_siblings_become_read() {
    let ws = workspace();
    let (_dir, mut a) = ApplyHarness::new(&ws);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_row_status(&mut background, "sess-1", AgentStatus::Success);
    set_row_status(&mut background, "sess-2", AgentStatus::Success);
    set_all_rows_unread(&mut background);
    append_agent_row(&mut background, "sess-3", "%3", AgentStatus::Waiting, false);
    set_viewed(&mut background, false);
    a.apply(background);

    let (mut viewed, _, _) = two_pane_snapshot(&ws, first);
    set_row_status(&mut viewed, "sess-1", AgentStatus::Success);
    set_row_status(&mut viewed, "sess-2", AgentStatus::Success);
    set_all_rows_unread(&mut viewed);
    append_agent_row(&mut viewed, "sess-3", "%3", AgentStatus::Waiting, false);
    a.apply(viewed.clone());
    force_tab_dwell_elapsed(&mut a);
    a.apply(viewed);

    assert_eq!(
        row_ids(&a.current),
        vec!["sess-3", "sess-1", "sess-2"],
        "tab-view read clears do not let unread siblings pin above a waiting row"
    );
    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert!(!row_unread_by_id(&a.current, "sess-2"));
}

#[test]
fn tab_switch_out_before_dwell_leaves_siblings_unread() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime, instance_a);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_all_rows_unread(&mut background);
    set_viewed(&mut background, false);
    a.apply(background);

    let (mut viewed, _, _) = two_pane_snapshot(&ws, first);
    set_all_rows_unread(&mut viewed);
    a.apply(viewed);
    assert!(a.tab_read_dwell_until.is_some());

    let mut left = a.current.clone();
    set_viewed(&mut left, false);
    a.apply(left);

    assert_eq!(a.tab_read_dwell_until, None);
    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert!(row_unread_by_id(&a.current, "sess-2"));
    let marks = a.read_marks.load_merged();
    assert!(marks.cleared_at_ms("sess-1").is_some());
    assert!(marks.cleared_at_ms("sess-2").is_none());
}

#[test]
fn staying_on_tab_does_not_sweep_new_unread() {
    let ws = workspace();
    let (_dir, mut a) = ApplyHarness::new(&ws);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_all_rows_unread(&mut background);
    set_viewed(&mut background, false);
    a.apply(background);
    let (mut viewed, _, _) = two_pane_snapshot(&ws, first.clone());
    set_all_rows_unread(&mut viewed);
    a.apply(viewed);

    let (mut still_viewed, _, _) = two_pane_snapshot(&ws, first);
    still_viewed.worktree_groups[0].rows[1].unread = true;
    still_viewed.worktree_groups[0].rows[1].last_activity = fixed_time(1_700_000_200);
    a.apply(still_viewed);

    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert!(row_unread_by_id(&a.current, "sess-2"));
}

#[test]
fn new_unread_during_order_hold_appends_after_held_rows() {
    let ws = workspace();
    let (_dir, mut a) = ApplyHarness::new(&ws);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_row_status(&mut background, "sess-1", AgentStatus::Success);
    set_row_status(&mut background, "sess-2", AgentStatus::Running);
    background.worktree_groups[0].rows[0].unread = true;
    set_viewed(&mut background, false);
    a.apply(background);

    let (mut viewed, _, _) = two_pane_snapshot(&ws, first.clone());
    set_row_status(&mut viewed, "sess-1", AgentStatus::Success);
    set_row_status(&mut viewed, "sess-2", AgentStatus::Running);
    viewed.worktree_groups[0].rows[0].unread = true;
    a.apply(viewed);

    let (mut with_new_unread, _, _) = two_pane_snapshot(&ws, first);
    set_row_status(&mut with_new_unread, "sess-1", AgentStatus::Success);
    set_row_status(&mut with_new_unread, "sess-2", AgentStatus::Running);
    append_agent_row(
        &mut with_new_unread,
        "sess-new",
        "%9",
        AgentStatus::Waiting,
        true,
    );
    a.apply(with_new_unread);

    assert_eq!(
        row_ids(&a.current),
        vec!["sess-1", "sess-2", "sess-new"],
        "fresh unread rows blink and feed the banner without reshuffling the frozen list"
    );
    assert!(row_unread_by_id(&a.current, "sess-new"));
}

#[test]
fn attach_to_viewed_tab_keeps_unread_siblings() {
    let ws = workspace();
    let (_dir, mut a) = ApplyHarness::new(&ws);
    let (mut viewed, _, _) = two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_all_rows_unread(&mut viewed);

    a.apply(viewed);

    assert_eq!(a.ui.viewing_own_tab, Some(true));
    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert!(row_unread_by_id(&a.current, "sess-2"));
    let marks = a.read_marks.load_merged();
    assert!(marks.cleared_at_ms("sess-1").is_some());
    assert!(marks.cleared_at_ms("sess-2").is_none());
}

#[test]
fn frameless_fold_does_not_blip_switch_in() {
    let ws = workspace();
    let (_dir, mut a) = ApplyHarness::new(&ws);
    let (mut background, first, _) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    set_all_rows_unread(&mut background);
    set_viewed(&mut background, false);
    a.apply(background);
    let (mut viewed, _, _) = two_pane_snapshot(&ws, first.clone());
    set_all_rows_unread(&mut viewed);
    a.apply(viewed);

    let mut frameless = a.current.clone();
    frameless.own_view = None;
    a.apply_outcome(FetchUpdate::Snapshot {
        snapshot: Box::new(frameless),
        role: FetchRole::Producer,
        phase: FetchPhase::Interim,
        pane_frame: PaneFrame::Held,
    });
    assert_eq!(a.ui.viewing_own_tab, Some(true));

    let (mut still_viewed, _, _) = two_pane_snapshot(&ws, first);
    still_viewed.worktree_groups[0].rows[1].unread = true;
    still_viewed.worktree_groups[0].rows[1].last_activity = fixed_time(1_700_000_200);
    a.apply(still_viewed);

    assert!(!row_unread_by_id(&a.current, "sess-1"));
    assert!(row_unread_by_id(&a.current, "sess-2"));
}

#[test]
fn manual_unread_guard_suppresses_focused_read_until_revisit() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime.clone(), instance_a.clone());
    let read_marks = runtime.sidebar_read_marks_path(&instance_a);

    a.ui.unread_guard = Some("sess-1".to_owned());
    let mut still_focused =
        row_snapshot_at(&ws, AgentStatus::Success, true, fixed_time(1_700_000_000));
    still_focused.worktree_groups[0].rows[0].unread = true;

    a.apply(still_focused);

    assert!(row_unread(&a.current));
    assert_eq!(a.ui.unread_guard.as_deref(), Some("sess-1"));
    assert!(
        !read_marks.exists(),
        "the guarded focused row must not write a read receipt"
    );

    let mut focus_left =
        row_snapshot_at(&ws, AgentStatus::Success, false, fixed_time(1_700_000_000));
    focus_left.worktree_groups[0].rows[0].unread = true;
    a.apply(focus_left);

    assert!(row_unread(&a.current));
    assert_eq!(a.ui.unread_guard, None);
    assert!(
        !read_marks.exists(),
        "leaving focus only releases the guard; it does not clear the row"
    );

    let mut refocused = row_snapshot_at(&ws, AgentStatus::Success, true, fixed_time(1_700_000_000));
    refocused.worktree_groups[0].rows[0].unread = true;
    a.apply(refocused);

    assert!(
        read_marks.exists(),
        "revisiting the row writes the normal focused read receipt"
    );
    assert!(!row_unread(&a.current));
}

#[test]
fn non_final_fast_success_keeps_refresh_alert_active() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    h.health = degraded_health("snapshot failed: produce");

    let applied = h.apply_outcome(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot(&ws)),
        role: FetchRole::Producer,
        phase: FetchPhase::Interim,
        pane_frame: PaneFrame::Held,
    });

    assert!(!applied.should_exit);
    assert!(!applied.tab_emptied);
    assert!(!applied.rejected);
    assert_eq!(h.health.failure_streak, ALERT_AFTER_FAILURES);
    assert!(
        h.health
            .alert
            .as_ref()
            .is_some_and(|alert| alert.is_active()),
        "only a final success may mark the refresh loop recovered"
    );
}

#[test]
fn gate_hold_notice_arms_and_clears_with_gate_state() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    h.current = row_snapshot(&ws, AgentStatus::Running, false);
    h.current.panes_produced_at_ms = Some(10);
    let mut empty = snapshot(&ws);
    empty.panes_produced_at_ms = Some(11);

    let held = h.apply(empty);
    assert!(held.rejected);
    assert!(matches!(
        h.ui.gate_notice,
        Some(GateNotice {
            rule: GateRule::EmptyStampedFrame,
        })
    ));

    let mut recovered = h.current.clone();
    recovered.panes_produced_at_ms = Some(12);
    let accepted = h.apply(recovered);
    assert!(!accepted.rejected);
    assert!(h.ui.gate_notice.is_none());
}

#[test]
fn diagnostics_record_fetch_and_gate_transitions() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), ws.clone(), "rimz-test", None);
    let mut prev = row_snapshot(&ws, AgentStatus::Running, false);
    prev.panes_produced_at_ms = Some(10);
    let mut incoming = prev.clone();
    incoming.panes_produced_at_ms = Some(11);
    let held_gate = GateState {
        reject_streak: 2,
        rejecting_since: Some(fixed_time(1_700_000_000)),
        spend_carry_since: None,
        rule: Some(GateRule::EmptyStampedFrame),
    };

    emit_diagnostics(
        &sink,
        FetchDiagnostics {
            prev_snapshot: &prev,
            incoming_panes_produced_at_ms: prev.panes_produced_at_ms,
            next_snapshot: &prev,
            prev_health: &Health::default(),
            next_health: &Health {
                failure_streak: 1,
                alert: None,
            },
            prev_gate: &held_gate,
            next_gate: &held_gate,
            fetch_failure: Some("pane discovery failed".to_owned()),
            rejected: false,
            released_via_escape_hatch: false,
            is_elder: true,
            now: fixed_time(1_700_000_000),
        },
    );
    emit_diagnostics(
        &sink,
        FetchDiagnostics {
            prev_snapshot: &prev,
            incoming_panes_produced_at_ms: incoming.panes_produced_at_ms,
            next_snapshot: &prev,
            prev_health: &Health::default(),
            next_health: &Health::default(),
            prev_gate: &GateState::default(),
            next_gate: &held_gate,
            fetch_failure: None,
            rejected: true,
            released_via_escape_hatch: false,
            is_elder: true,
            now: fixed_time(1_700_000_001),
        },
    );
    emit_diagnostics(
        &sink,
        FetchDiagnostics {
            prev_snapshot: &prev,
            incoming_panes_produced_at_ms: prev.panes_produced_at_ms,
            next_snapshot: &prev,
            prev_health: &Health::default(),
            next_health: &Health::default(),
            prev_gate: &held_gate,
            next_gate: &GateState::default(),
            fetch_failure: None,
            rejected: false,
            released_via_escape_hatch: true,
            is_elder: true,
            now: fixed_time(1_700_000_003),
        },
    );

    let events = diagnostic_events(&sink);
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        DiagEvent::FetchFailure {
            reason,
            failure_streak: 1,
        } if reason == "pane discovery failed"
    ));
    assert!(matches!(
        &events[1],
        DiagEvent::GateHold {
            rule: GateRule::EmptyStampedFrame,
            prev_produced_at_ms: Some(10),
            incoming_produced_at_ms: Some(11),
            reject_streak: 2,
        }
    ));
    assert!(matches!(
        &events[2],
        DiagEvent::GateRelease {
            rule: GateRule::EmptyStampedFrame,
            held_ms: 3_000,
            via_escape_hatch: true,
        }
    ));
}
