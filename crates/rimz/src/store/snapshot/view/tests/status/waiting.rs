use super::*;

#[test]
fn codex_auto_permission_card_and_ladder_wait_for_review() {
    for (age, expected, phase) in [
        (59, AgentStatus::Running, TurnPhase::Reasoning),
        (60, AgentStatus::Waiting, TurnPhase::Idle),
    ] {
        let mut session = agent("codex", "review", AgentStatus::Waiting, 0)
            .worktree("/repo/main")
            .active_ago(age);
        session.mode = Some(crate::agents::PermissionMode::Auto);
        session.waiting_since = Some(ago(age));
        session.open_ask = Some(crate::agents::OpenAsk {
            id: crate::ids::AskId::new(),
            kind: crate::agents::AskKind::Permission,
            detail: Some("shell: cargo test".to_owned()),
            native_key: None,
            since: ago(age),
        });
        let seed = crate::store::snapshot::AgentCard::from_agent(&session, epoch());
        assert_eq!(seed.status, expected, "seed at {age}s");
        assert_eq!(seed.phase, phase);
        assert_eq!(seed.open_ask().is_some(), age == 60);

        let mut child = child_state("review", "child", AgentStatus::Running, 0);
        child.kind = AgentKind::new_unchecked("codex");
        let snapshot = room_with_agent_panes(vec![session, child]);
        let row = row(&snapshot, "review");
        if age == 59 {
            assert_eq!(row.last_activity, epoch());
        }
        assert_eq!(row.status(), Some(expected), "ladder at {age}s");
        assert_eq!(row.phase(), phase);
        let source = snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == "review")
            .unwrap();
        let needs = crate::store::snapshot::needs_you(
            source,
            Some(row),
            &crate::agents::ParkDemotion::default(),
            epoch(),
        );
        assert_eq!(needs.is_some(), age == 60);
        if let Some(needs) = needs {
            assert_eq!(needs.since, epoch());
        }
    }
}

#[test]
fn awaiting_input_outranks_derived_statuses() {
    let mut session = agent("claude", "claude-ask", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(default_stall_secs() + 60)
        .turn_error(10, "API Error: Bad Request");
    session.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::agents::BudgetWindow::Session,
        at: epoch(),
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "claude-ask");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Waiting),
        "a current native prompt outranks budget, turn-error, and stall projections"
    );
    assert_eq!(
        row.turn_error_label(),
        None,
        "an untouched waiting row receives no derived error label"
    );
}

#[test]
fn answered_waiting_reads_running() {
    let mut session = agent("claude", "claude-ask", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(5);
    session.waiting_since = Some(ago(60));

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "claude-ask");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "newer activity proves the keyless ask was answered"
    );
    assert_eq!(
        row.phase(),
        TurnPhase::Reasoning,
        "an answered ask resumes reasoning until a durable clear arrives"
    );
}

#[test]
fn stale_waiting_with_live_child_projects_running() {
    let mut session = agent("claude", "root", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(5);
    session.waiting_since = Some(ago(60));

    let snapshot = room_with_agent_panes(vec![
        session,
        child_state("root", "child", AgentStatus::Running, 5),
    ]);
    let row = row(&snapshot, "root");
    assert_eq!(row.status(), Some(AgentStatus::Running));
    assert_eq!(
        row.phase(),
        TurnPhase::Reasoning,
        "the live-child rung makes stale Waiting's Running promotion observable"
    );
}

#[test]
fn stale_waiting_uses_running_stall_outcome() {
    let stalled_secs = default_stall_secs() + 60;
    let mut session = agent("claude", "failed", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(stalled_secs);
    session.waiting_since = Some(ago(stalled_secs + 60));

    let failed = room_with_agent_panes(vec![session]);
    assert_eq!(
        row(&failed, "failed").status(),
        Some(AgentStatus::Failed),
        "stale waiting without a spent window fails like stalled running"
    );
}
