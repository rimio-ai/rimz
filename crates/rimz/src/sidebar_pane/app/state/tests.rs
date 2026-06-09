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
    )
    .expect("apply empty-tab frame");

    assert!(second.should_exit);
    assert!(second.tab_emptied);
    assert!(!second.rejected);
}
