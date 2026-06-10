use super::*;
use crate::sidebar_pane::app::fixtures::{snapshot, workspace};
use crate::sidebar_pane::app::health::ALERT_AFTER_FAILURES;
use crate::sidebar_pane::render::Alert;
use crate::{AgentCard, PaneId, RowCard, SidebarStatusCount, SidebarWorktreeGroup};

/// Health seeded with a live alert, as if a failure already crossed the
/// debounce threshold — the starting point for recovery/sticky tests.
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
        instance_id: crate::SidebarInstanceId::new(),
        tick_seconds: 1,
        refresh_ms_override: None,
        notification_prefs: crate::config::NotificationsPrefs::default(),
        own_pane: None,
    }
}

fn snapshot_with_sibling_count(ws: &WorkspaceId, sibling_count: usize) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.own_view = Some(crate::SidebarOwnView {
        sibling_count,
        own_is_active: false,
        active_pane_id: None,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    });
    snapshot
}

fn fixed_time(second: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(second).expect("fixed timestamp")
}

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<crate::schema::diag::DiagEvent> {
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

fn row_snapshot(
    ws: &WorkspaceId,
    status: crate::feed::AgentStatus,
    focused: bool,
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
            last_activity: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
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
            own_view_is_daemon: false,
        });
    }
    snap
}

fn apply_ok(
    config: &ServeConfig,
    snapshot: SidebarSnapshot,
    last_snapshot: &mut Option<SidebarSnapshot>,
    current: &mut SidebarSnapshot,
    ui: &mut UiState,
) {
    let mut health = Health::default();
    let mut gate = GateState::default();
    let mut self_close = SelfCloseState::default();
    apply_fetch_outcome(
        config,
        FetchOutcome {
            snapshot: Ok(snapshot),
            final_for_request: true,
            fresh_pane_frame: true,
        },
        last_snapshot,
        current,
        &mut health,
        &mut gate,
        &mut self_close,
        ui,
        std::time::Instant::now(),
        None,
    )
    .expect("apply ok fetch");
}

#[test]
fn first_ok_fetch_clears_status_and_records_snapshot() {
    let ws = workspace();
    let snap = snapshot(&ws);
    let state = compute_next_state(&ws, None, Ok(snap.clone()), None, &Health::default());
    assert!(state.health.alert.is_none());
    assert_eq!(state.health.failure_streak, 0);
    assert!(state.last_snapshot.is_some());
    assert_eq!(state.snapshot.workspace_id, ws);
}

#[test]
fn unread_marks_done_transition_and_focus_clears_it() {
    let ws = workspace();
    let config = serve_config(&ws);
    let mut last_snapshot = None;
    let mut current = snapshot(&ws);
    let mut ui = UiState::default();

    apply_ok(
        &config,
        row_snapshot(&ws, crate::feed::AgentStatus::Running, false),
        &mut last_snapshot,
        &mut current,
        &mut ui,
    );
    assert!(!current.worktree_groups[0].rows[0].unread);

    apply_ok(
        &config,
        row_snapshot(&ws, crate::feed::AgentStatus::Success, false),
        &mut last_snapshot,
        &mut current,
        &mut ui,
    );
    assert!(current.worktree_groups[0].rows[0].unread);

    apply_ok(
        &config,
        row_snapshot(&ws, crate::feed::AgentStatus::Success, true),
        &mut last_snapshot,
        &mut current,
        &mut ui,
    );
    assert!(!current.worktree_groups[0].rows[0].unread);
}

#[test]
fn single_failure_is_absorbed_without_an_alert() {
    // One flaky tick must not flash a banner: the streak climbs but no
    // alert arms yet, and the last good frame is reused.
    let ws = workspace();
    let previous = snapshot(&ws);
    let state = compute_next_state(
        &ws,
        None,
        Err("ledger not found".to_owned()),
        Some(previous.clone()),
        &Health::default(),
    );
    assert!(state.health.alert.is_none(), "one blip must not alarm");
    assert_eq!(state.health.failure_streak, 1);
    assert!(state.last_snapshot.is_some());
    assert_eq!(state.snapshot.workspace_id, previous.workspace_id);
}

#[test]
fn sustained_failure_raises_active_alert_after_threshold() {
    let ws = workspace();
    let previous = snapshot(&ws);
    let first = compute_next_state(
        &ws,
        None,
        Err("ledger not found".to_owned()),
        Some(previous.clone()),
        &Health::default(),
    );
    let second = compute_next_state(
        &ws,
        None,
        Err("ledger not found".to_owned()),
        first.last_snapshot,
        &first.health,
    );
    let alert = second.health.alert.expect("a sustained failure alerts");
    assert!(alert.is_active());
    assert!(alert.reason.contains("snapshot failed"));
    assert!(alert.reason.contains("ledger not found"));
    assert!(second.last_snapshot.is_some());
}

#[test]
fn sustained_failure_without_previous_snapshot_uses_placeholder() {
    let ws = workspace();
    let err = || Err::<SidebarSnapshot, String>("ledger not found".to_owned());
    let first = compute_next_state(&ws, None, err(), None, &Health::default());
    let second = compute_next_state(&ws, None, err(), None, &first.health);
    assert!(second.health.alert.is_some_and(|alert| alert.is_active()));
    assert!(second.last_snapshot.is_none());
    assert_eq!(second.snapshot.workspace_id, ws);
    assert!(second.snapshot.needs_attention.is_empty());
}

#[test]
fn sustained_heartbeat_failure_alerts_but_keeps_fresh_snapshot() {
    let ws = workspace();
    let snap = snapshot(&ws);
    let first = compute_next_state(
        &ws,
        Some("hb failed".to_owned()),
        Ok(snap.clone()),
        None,
        &Health::default(),
    );
    let second = compute_next_state(
        &ws,
        Some("hb failed".to_owned()),
        Ok(snap.clone()),
        first.last_snapshot,
        &first.health,
    );
    let alert = second
        .health
        .alert
        .expect("sustained heartbeat failure alerts");
    assert!(alert.reason.contains("heartbeat failed"));
    // Heartbeat failing does not invalidate a fresh snapshot.
    assert!(second.last_snapshot.is_some());
}

#[test]
fn active_alert_since_stays_pinned_across_the_episode() {
    let ws = workspace();
    let armed = degraded_health("snapshot failed: first");
    let first_since = armed.alert.as_ref().unwrap().since;
    let next = compute_next_state(
        &ws,
        None,
        Err("second".to_owned()),
        Some(snapshot(&ws)),
        &armed,
    );
    let alert = next.health.alert.expect("still degraded");
    assert_eq!(alert.since, first_since, "since must remain pinned");
    assert!(alert.reason.contains("second"));
}

#[test]
fn recovery_marks_alert_recovered_and_keeps_it_sticky() {
    // Recovery does not erase the alert: it lingers, recovered, until the
    // user dismisses it.
    let ws = workspace();
    let armed = degraded_health("snapshot failed: x");
    let recovered = compute_next_state(&ws, None, Ok(snapshot(&ws)), None, &armed);
    let alert = recovered.health.alert.expect("recovered alert lingers");
    assert!(!alert.is_active());
    assert!(alert.recovered_at.is_some());
    assert_eq!(recovered.health.failure_streak, 0);
}

#[test]
fn non_final_fast_success_does_not_recover_refresh_health() {
    let ws = workspace();
    let config = serve_config(&ws);
    let mut last_snapshot = Some(snapshot(&ws));
    let mut current = snapshot(&ws);
    let mut health = degraded_health("snapshot failed: produce");
    let mut gate = GateState::default();
    let mut self_close = SelfCloseState::default();
    let mut ui = UiState::default();

    let applied = apply_fetch_outcome(
        &config,
        FetchOutcome {
            snapshot: Ok(snapshot(&ws)),
            final_for_request: false,
            fresh_pane_frame: false,
        },
        &mut last_snapshot,
        &mut current,
        &mut health,
        &mut gate,
        &mut self_close,
        &mut ui,
        std::time::Instant::now(),
        None,
    )
    .expect("apply non-final fast frame");

    assert!(!applied.should_exit);
    assert!(!applied.tab_emptied);
    assert!(!applied.rejected);
    assert_eq!(health.failure_streak, ALERT_AFTER_FAILURES);
    assert!(
        health.alert.as_ref().is_some_and(|alert| alert.is_active()),
        "only a final success may mark the refresh loop recovered"
    );
}

#[test]
fn self_close_outcome_marks_tab_emptied() {
    let ws = workspace();
    let config = serve_config(&ws);
    let mut last_snapshot = Some(snapshot(&ws));
    let mut current = snapshot(&ws);
    let mut health = Health::default();
    let mut gate = GateState::default();
    let mut self_close = SelfCloseState::default();
    let mut ui = UiState::default();
    let anim_start = std::time::Instant::now();

    let first = apply_fetch_outcome(
        &config,
        FetchOutcome {
            snapshot: Ok(snapshot_with_sibling_count(&ws, 1)),
            final_for_request: true,
            fresh_pane_frame: true,
        },
        &mut last_snapshot,
        &mut current,
        &mut health,
        &mut gate,
        &mut self_close,
        &mut ui,
        anim_start,
        None,
    )
    .expect("apply sibling frame");

    assert!(!first.should_exit);
    assert!(!first.tab_emptied);

    let second = apply_fetch_outcome(
        &config,
        FetchOutcome {
            snapshot: Ok(snapshot_with_sibling_count(&ws, 0)),
            final_for_request: true,
            fresh_pane_frame: true,
        },
        &mut last_snapshot,
        &mut current,
        &mut health,
        &mut gate,
        &mut self_close,
        &mut ui,
        anim_start,
        None,
    )
    .expect("apply empty-tab frame");

    assert!(second.should_exit);
    assert!(second.tab_emptied);
    assert!(!second.rejected);
}

#[test]
fn diagnostics_do_not_release_gate_on_fetch_failure() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), ws.clone(), "rimz-test", None);
    let snapshot = row_snapshot(&ws, crate::feed::AgentStatus::Running, false);
    let gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(jiff::Timestamp::now()),
        rule: Some(crate::schema::diag::GateRule::EmptyStampedFrame),
    };

    emit_diagnostics(
        Some(&sink),
        &snapshot,
        &snapshot,
        &snapshot,
        &Health::default(),
        &Health::default(),
        &gate,
        &gate,
        Some("pane discovery failed".to_owned()),
        false,
        false,
        jiff::Timestamp::now(),
    );

    let text = std::fs::read_to_string(sink.log_path()).expect("diagnostic log");
    assert!(text.contains("\"kind\":\"fetch_failure\""));
    assert!(
        !text.contains("\"kind\":\"gate_release\""),
        "a fetch failure does not mean a held regression was released: {text}"
    );
}

#[test]
fn diagnostics_record_gate_hold_and_release_details() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), ws.clone(), "rimz-test", None);
    let mut prev = row_snapshot(&ws, crate::feed::AgentStatus::Running, false);
    prev.panes_produced_at_ms = Some(10);
    let mut incoming = prev.clone();
    incoming.panes_produced_at_ms = Some(11);
    let next_gate = GateState {
        reject_streak: 2,
        rejecting_since: Some(fixed_time(1_700_000_000)),
        rule: Some(crate::schema::diag::GateRule::EmptyStampedFrame),
    };

    emit_diagnostics(
        Some(&sink),
        &prev,
        &incoming,
        &prev,
        &Health::default(),
        &Health::default(),
        &GateState::default(),
        &next_gate,
        None,
        true,
        false,
        fixed_time(1_700_000_001),
    );

    let events = diagnostic_events(&sink);
    assert!(matches!(
        &events[..],
        [crate::schema::diag::DiagEvent::GateHold {
            rule: crate::schema::diag::GateRule::EmptyStampedFrame,
            prev_produced_at_ms: Some(10),
            incoming_produced_at_ms: Some(11),
            reject_streak: 2,
        }]
    ));

    emit_diagnostics(
        Some(&sink),
        &prev,
        &prev,
        &prev,
        &Health::default(),
        &Health::default(),
        &next_gate,
        &GateState::default(),
        None,
        false,
        true,
        fixed_time(1_700_000_003),
    );

    let events = diagnostic_events(&sink);
    assert!(matches!(
        &events[1],
        crate::schema::diag::DiagEvent::GateRelease {
            rule: crate::schema::diag::GateRule::EmptyStampedFrame,
            held_ms: 3_000,
            via_escape_hatch: true,
        }
    ));
}

#[test]
fn gate_hold_notice_arms_and_clears_with_gate_state() {
    let ws = workspace();
    let config = serve_config(&ws);
    let mut current = row_snapshot(&ws, crate::feed::AgentStatus::Running, false);
    current.panes_produced_at_ms = Some(10);
    let mut last_snapshot = Some(current.clone());
    let mut health = Health::default();
    let mut gate = GateState::default();
    let mut self_close = SelfCloseState::default();
    let mut ui = UiState::default();
    let mut empty = snapshot(&ws);
    empty.panes_produced_at_ms = Some(11);

    let held = apply_fetch_outcome(
        &config,
        FetchOutcome {
            snapshot: Ok(empty),
            final_for_request: true,
            fresh_pane_frame: true,
        },
        &mut last_snapshot,
        &mut current,
        &mut health,
        &mut gate,
        &mut self_close,
        &mut ui,
        std::time::Instant::now(),
        None,
    )
    .expect("apply held frame");

    assert!(held.rejected);
    assert!(matches!(
        ui.gate_notice,
        Some(crate::sidebar_pane::render::GateNotice {
            rule: crate::schema::diag::GateRule::EmptyStampedFrame,
        })
    ));

    let mut recovered = current.clone();
    recovered.panes_produced_at_ms = Some(12);
    let accepted = apply_fetch_outcome(
        &config,
        FetchOutcome {
            snapshot: Ok(recovered),
            final_for_request: true,
            fresh_pane_frame: true,
        },
        &mut last_snapshot,
        &mut current,
        &mut health,
        &mut gate,
        &mut self_close,
        &mut ui,
        std::time::Instant::now(),
        None,
    )
    .expect("apply recovered frame");

    assert!(!accepted.rejected);
    assert!(ui.gate_notice.is_none());
}

#[test]
fn diagnostics_record_health_alert_recovery_inside_rate_limit_window() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), ws.clone(), "rimz-test", None);
    let snapshot = row_snapshot(&ws, crate::feed::AgentStatus::Running, false);
    let since = fixed_time(1_700_000_000);
    let active = Health {
        failure_streak: crate::sidebar_pane::app::health::ALERT_AFTER_FAILURES,
        alert: Some(Alert::active("snapshot failed: pane discovery", since)),
    };
    let recovered = Health {
        failure_streak: 0,
        alert: Some(Alert {
            reason: "snapshot failed: pane discovery".to_owned(),
            since,
            recovered_at: Some(fixed_time(1_700_000_001)),
        }),
    };

    emit_diagnostics(
        Some(&sink),
        &snapshot,
        &snapshot,
        &snapshot,
        &Health::default(),
        &active,
        &GateState::default(),
        &GateState::default(),
        None,
        false,
        false,
        fixed_time(1_700_000_000),
    );
    emit_diagnostics(
        Some(&sink),
        &snapshot,
        &snapshot,
        &snapshot,
        &active,
        &recovered,
        &GateState::default(),
        &GateState::default(),
        None,
        false,
        false,
        fixed_time(1_700_000_001),
    );

    let events = diagnostic_events(&sink);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        crate::schema::diag::DiagEvent::HealthAlert {
            reason,
            since_ms,
            recovered_after_ms: None,
        } if reason == "snapshot failed: pane discovery"
            && *since_ms == since.as_millisecond() as u64
    ));
    assert!(matches!(
        &events[1],
        crate::schema::diag::DiagEvent::HealthAlert {
            reason,
            since_ms,
            recovered_after_ms: Some(1_000),
        } if reason == "snapshot failed: pane discovery"
            && *since_ms == since.as_millisecond() as u64
    ));
}

#[test]
fn diagnostics_stay_silent_for_stable_group_location() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), ws.clone(), "rimz-test", None);
    let snapshot = row_snapshot(&ws, crate::feed::AgentStatus::Running, false);

    emit_diagnostics(
        Some(&sink),
        &snapshot,
        &snapshot,
        &snapshot,
        &Health::default(),
        &Health::default(),
        &GateState::default(),
        &GateState::default(),
        None,
        false,
        false,
        fixed_time(1_700_000_000),
    );

    assert!(
        !sink.log_path().exists(),
        "stable snapshots should not emit a group-migration diagnostic"
    );
}
