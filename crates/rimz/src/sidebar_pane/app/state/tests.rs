use super::*;
use crate::feed::AgentStatus;
use crate::schema::diag::{DiagEvent, GateRule};
use crate::sidebar::read_marks::ReadMarkStore;
use crate::sidebar_pane::app::fixtures::{snapshot, workspace};
use crate::sidebar_pane::app::health::ALERT_AFTER_FAILURES;
use crate::sidebar_pane::render::{Alert, GateNotice};
use crate::{
    AgentCard, PaneId, RowCard, RuntimePaths, SidebarInstanceId, SidebarStatusCount,
    SidebarWorktreeGroup,
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
        notification_prefs: crate::config::NotificationsPrefs::default(),
        own_pane: None,
    }
}

fn fixed_time(second: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(second).expect("fixed timestamp")
}

fn fetch_failed(
    ws: &WorkspaceId,
    reason: &str,
    previous: Option<SidebarSnapshot>,
    health: &Health,
) -> RenderState {
    compute_next_state(ws, None, Err(reason.to_owned()), previous, health)
}

fn active_alert(health: &Health) -> &Alert {
    let alert = health.alert.as_ref().expect("active alert");
    assert!(alert.is_active());
    alert
}

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<DiagEvent> {
    std::fs::read_to_string(sink.log_path())
        .expect("diagnostic log")
        .lines()
        .map(|line| {
            serde_json::from_str::<crate::schema::diag::DiagEnvelope>(line)
                .expect("diagnostic envelope")
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
            pane: Some(crate::feed::PaneRef::from_id(pane_id.clone())),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            unread: false,
            last_activity,
            card: RowCard::Agent(Box::new(AgentCard {
                status: Some(status),
                phase: crate::agents::TurnPhase::Idle,
                ..AgentCard::default()
            })),
        }],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];
    if focused {
        snap.own_view = Some(crate::SidebarOwnView {
            sibling_count: 2,
            own_is_active: false,
            active_pane_id: Some(pane_id),
            working_pane_ids: Vec::new(),
            focus_contested: false,
            own_view_is_daemon: false,
        });
    }
    snap
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

struct ApplyHarness {
    config: ServeConfig,
    last_snapshot: Option<SidebarSnapshot>,
    current: SidebarSnapshot,
    health: Health,
    gate: GateState,
    self_close: SelfCloseState,
    ui: UiState,
    read_marks: ReadMarkStore,
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
        Self {
            config,
            last_snapshot: None,
            current: snapshot(ws),
            health: Health::default(),
            gate: GateState::default(),
            self_close: SelfCloseState::default(),
            ui: UiState::default(),
            read_marks: ReadMarkStore::new(runtime, instance_id),
        }
    }

    fn apply(&mut self, snapshot: SidebarSnapshot) -> ApplyOutcome {
        self.apply_outcome(FetchOutcome {
            snapshot: Ok(snapshot),
            final_for_request: true,
            fresh_pane_frame: true,
        })
    }

    fn apply_outcome(&mut self, outcome: FetchOutcome) -> ApplyOutcome {
        apply_fetch_outcome(
            &self.config,
            outcome,
            &mut self.last_snapshot,
            &mut self.current,
            &mut self.health,
            &mut self.gate,
            &mut self.self_close,
            &mut self.ui,
            &mut self.read_marks,
            std::time::Instant::now(),
            None,
        )
        .expect("apply fetch outcome")
    }
}

fn two_pane_snapshot(
    ws: &WorkspaceId,
    active: PaneId,
    focus_contested: bool,
) -> (SidebarSnapshot, PaneId, PaneId) {
    let first = PaneId::from_parts(crate::MuxName::Tmux, "%1");
    let second = PaneId::from_parts(crate::MuxName::Tmux, "%2");
    let mut snap = row_snapshot(ws, crate::feed::AgentStatus::Running, false);
    let mut second_row = snap.worktree_groups[0].rows[0].clone();
    second_row.id = "sess-2".to_owned();
    second_row.name = "codex".to_owned();
    second_row.pane = Some(crate::feed::PaneRef::from_id(second.clone()));
    snap.worktree_groups[0].rows.push(second_row);
    snap.worktree_groups[0].status_counts[0].count = 2;
    snap.own_view = Some(crate::SidebarOwnView {
        sibling_count: 2,
        own_is_active: false,
        active_pane_id: Some(active),
        working_pane_ids: vec![first.clone(), second.clone()],
        focus_contested,
        own_view_is_daemon: false,
    });
    (snap, first, second)
}

#[test]
fn contested_own_view_holds_existing_selection_baseline() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    let (initial, first, second) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"), false);

    h.apply(initial);
    assert_eq!(h.ui.baseline_pane, Some(first.clone()));

    let (contested, _, _) = two_pane_snapshot(&ws, second, true);
    h.apply(contested);

    assert_eq!(h.ui.baseline_pane, Some(first.clone()));
    assert_eq!(h.ui.selected_pane, Some(first));
}

#[test]
fn focus_event_resolves_contest_then_republished_contest_holds_clicked_baseline() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    let (initial, first, second) =
        two_pane_snapshot(&ws, PaneId::from_parts(crate::MuxName::Tmux, "%1"), false);

    h.apply(initial);
    assert_eq!(h.ui.selected_pane, Some(first.clone()));

    let (mut contested, _, _) = two_pane_snapshot(&ws, first.clone(), true);
    contested.focus_contested_panes = vec![first.clone(), second.clone()];
    let mut events = crate::sidebar::events::EventStore::default();
    events.append(
        crate::schema::sidebar_event::SidebarEvent::FocusChanged {
            focused: vec![second.clone()],
            unfocused: Vec::new(),
        },
        10,
        10,
    );
    let fused = crate::sidebar::fuse::fuse(&contested, &events, 10);
    assert_eq!(
        fused
            .own_view
            .as_ref()
            .and_then(|view| view.active_pane_id.clone()),
        Some(second.clone())
    );
    assert!(
        !fused
            .own_view
            .as_ref()
            .is_some_and(|view| view.focus_contested),
        "the event resolves the contested own view for this fold",
    );

    h.apply(fused);
    assert_eq!(h.ui.selected_pane, Some(second.clone()));

    h.apply(contested);
    assert_eq!(
        h.ui.selected_pane,
        Some(second),
        "after the focus event expires, a still-contested pull holds the clicked baseline",
    );
}

#[test]
fn compute_next_state_keeps_frame_and_tracks_refresh_health() {
    let ws = workspace();
    let ok = compute_next_state(&ws, None, Ok(snapshot(&ws)), None, &Health::default());
    assert!(ok.health.alert.is_none());
    assert_eq!(ok.health.failure_streak, 0);
    assert!(ok.last_snapshot.is_some());
    assert_eq!(ok.snapshot.workspace_id, ws);

    let previous = snapshot(&ws);
    let first_failure = fetch_failed(
        &ws,
        "ledger not found",
        Some(previous.clone()),
        &Health::default(),
    );
    assert!(first_failure.health.alert.is_none());
    assert_eq!(first_failure.health.failure_streak, 1);
    assert_eq!(first_failure.snapshot.workspace_id, previous.workspace_id);
    assert!(first_failure.last_snapshot.is_some());

    let second_failure = fetch_failed(
        &ws,
        "ledger not found",
        first_failure.last_snapshot,
        &first_failure.health,
    );
    let alert = active_alert(&second_failure.health);
    assert!(alert.reason.contains("snapshot failed"));
    assert!(alert.reason.contains("ledger not found"));

    let cold_first = fetch_failed(&ws, "ledger not found", None, &Health::default());
    let cold_second = fetch_failed(&ws, "ledger not found", None, &cold_first.health);
    active_alert(&cold_second.health);
    assert!(cold_second.last_snapshot.is_none());
    assert_eq!(cold_second.snapshot.workspace_id, ws);
    assert!(cold_second.snapshot.needs_attention.is_empty());

    let heartbeat_first = compute_next_state(
        &ws,
        Some("hb failed".to_owned()),
        Ok(snapshot(&ws)),
        None,
        &Health::default(),
    );
    let heartbeat_second = compute_next_state(
        &ws,
        Some("hb failed".to_owned()),
        Ok(snapshot(&ws)),
        heartbeat_first.last_snapshot,
        &heartbeat_first.health,
    );
    let alert = active_alert(&heartbeat_second.health);
    assert!(alert.reason.contains("heartbeat failed"));
    assert!(heartbeat_second.last_snapshot.is_some());

    let armed = degraded_health("snapshot failed: first");
    let first_since = armed.alert.as_ref().unwrap().since;
    let still_degraded = fetch_failed(&ws, "second", Some(snapshot(&ws)), &armed);
    let alert = still_degraded.health.alert.expect("still degraded");
    assert_eq!(alert.since, first_since, "since must remain pinned");
    assert!(alert.reason.contains("second"));

    let recovered = compute_next_state(&ws, None, Ok(snapshot(&ws)), None, &armed);
    let alert = recovered.health.alert.expect("recovered alert lingers");
    assert!(!alert.is_active());
    assert!(alert.recovered_at.is_some());
    assert_eq!(recovered.health.failure_streak, 0);
}

#[test]
fn read_receipts_cross_instances_and_stay_episode_scoped() {
    let ws = workspace();
    let (_dir, runtime) = runtime_for(&ws);
    let instance_a = SidebarInstanceId::new();
    let mut a = ApplyHarness::for_runtime(&ws, runtime.clone(), instance_a.clone());
    let mut b = ApplyHarness::for_runtime(&ws, runtime.clone(), SidebarInstanceId::new());
    let old_stamp = fixed_time(1_700_000_000);
    let new_stamp = fixed_time(4_000_000_000);

    a.apply(row_snapshot_at(&ws, AgentStatus::Success, true, old_stamp));
    assert!(
        runtime.sidebar_read_marks_path(&instance_a).exists(),
        "a focused fresh renderer writes a read receipt"
    );
    assert!(!row_unread(&a.current));

    b.apply(row_snapshot_at(&ws, AgentStatus::Running, false, old_stamp));
    b.apply(row_snapshot_at(&ws, AgentStatus::Success, false, old_stamp));
    assert!(!row_unread(&b.current), "the peer consumes the receipt");

    b.apply(row_snapshot_at(&ws, AgentStatus::Running, false, new_stamp));
    b.apply(row_snapshot_at(&ws, AgentStatus::Success, false, new_stamp));
    assert!(
        row_unread(&b.current),
        "a later episode must not be cleared by the old receipt"
    );
}

#[test]
fn non_final_fast_success_keeps_refresh_alert_active() {
    let ws = workspace();
    let (_dir, mut h) = ApplyHarness::new(&ws);
    h.last_snapshot = Some(snapshot(&ws));
    h.health = degraded_health("snapshot failed: produce");

    let applied = h.apply_outcome(FetchOutcome {
        snapshot: Ok(snapshot(&ws)),
        final_for_request: false,
        fresh_pane_frame: false,
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
    h.last_snapshot = Some(h.current.clone());
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
        rule: Some(GateRule::EmptyStampedFrame),
    };

    emit_diagnostics(
        Some(&sink),
        &prev,
        &prev,
        &prev,
        &Health::default(),
        &Health {
            failure_streak: 1,
            alert: None,
        },
        &held_gate,
        &held_gate,
        Some("pane discovery failed".to_owned()),
        false,
        false,
        fixed_time(1_700_000_000),
    );
    emit_diagnostics(
        Some(&sink),
        &prev,
        &incoming,
        &prev,
        &Health::default(),
        &Health::default(),
        &GateState::default(),
        &held_gate,
        None,
        true,
        false,
        fixed_time(1_700_000_001),
    );
    emit_diagnostics(
        Some(&sink),
        &prev,
        &prev,
        &prev,
        &Health::default(),
        &Health::default(),
        &held_gate,
        &GateState::default(),
        None,
        false,
        true,
        fixed_time(1_700_000_003),
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
