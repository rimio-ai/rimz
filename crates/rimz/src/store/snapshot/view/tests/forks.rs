use super::*;

fn codex_lifecycle(
    secs_ago: u64,
    event_name: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    origin: Option<crate::agents::SessionOrigin>,
) -> crate::store::event::EventEnvelope {
    let mut observation =
        crate::agents::AgentLifecycleObservation::new(Some(agent_id.into()), signal);
    observation.pane_id = Some(crate::ids::PaneId::parse("tmux:%1").unwrap());
    observation.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "codex",
        4242,
        Some("process-start".to_owned()),
    ));
    observation.worktree_path = Some("/repo/main".to_owned());
    observation.origin = origin;
    let mut event = crate::store::event::EventEnvelope::agent_lifecycle(
        workspace(),
        "session",
        "codex",
        event_name,
        &observation,
    );
    event.timestamp = epoch() - std::time::Duration::from_secs(secs_ago);
    event
}

#[test]
fn late_tool_of_interrupted_turn_does_not_keep_the_old_pane_root_running() {
    let events = [
        codex_lifecycle(
            6,
            "SessionStart",
            "old",
            LifecycleSignal::Registered,
            Some(crate::agents::SessionOrigin::Fresh),
        ),
        codex_lifecycle(
            5,
            "UserPromptSubmit",
            "old",
            LifecycleSignal::TurnStarted,
            None,
        ),
        codex_lifecycle(
            4,
            "Interrupt",
            "old",
            LifecycleSignal::TurnInterrupted {
                turn_id: Some("turn-1".to_owned()),
            },
            None,
        ),
        codex_lifecycle(
            3,
            "PostToolUse",
            "old",
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                name: Some("Bash".to_owned()),
                native_key: None,
                turn_id: Some("turn-1".to_owned()),
            },
            None,
        ),
        codex_lifecycle(
            2,
            "SessionStart",
            "new",
            LifecycleSignal::Registered,
            Some(crate::agents::SessionOrigin::Fresh),
        ),
        codex_lifecycle(
            1,
            "UserPromptSubmit",
            "new",
            LifecycleSignal::TurnStarted,
            None,
        ),
        codex_lifecycle(
            0,
            "Stop",
            "new",
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            None,
        ),
    ];

    let agents = reduce_agent_states(&events);
    assert_eq!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "old")
            .unwrap()
            .status,
        AgentStatus::Idle,
        "the interrupted turn's trailing tool completion must not reopen it"
    );

    let mut snapshot = room(agents);
    snapshot.reap_stale_sessions();
    let snapshot = snapshot.with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);
    assert_eq!(rows(&snapshot).len(), 1);
    assert_eq!(rows(&snapshot)[0].id, "new");
    assert_eq!(rows(&snapshot)[0].status(), Some(AgentStatus::Success));
}

#[test]
fn same_pane_fork_folds_clocks_onto_the_primary_row() {
    let mut primary = agent("codex", "primary", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    primary.registered_at = Some(ago(600));
    primary.estimated_active_secs = Some(10);

    let mut fork = agent("codex", "fork", AgentStatus::Success, 2_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    fork.registered_at = Some(ago(60));
    fork.estimated_active_secs = Some(7);

    let snapshot =
        room(vec![primary, fork]).with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);
    let primary = row(&snapshot, "primary");

    assert_eq!(primary.last_activity, ago(5));
    assert_eq!(
        primary
            .as_agent()
            .and_then(|card| card.estimated_active_secs),
        Some(17)
    );
}

#[test]
fn bound_fork_folds_its_rested_primary_clock() {
    let mut primary = agent("codex", "primary", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    primary.registered_at = Some(ago(600));
    primary.estimated_active_secs = Some(10);

    let mut fork = agent("codex", "fork", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    fork.registered_at = Some(ago(60));
    fork.estimated_active_secs = Some(7);

    let snapshot =
        room(vec![primary, fork]).with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);
    let fork = row(&snapshot, "fork");

    assert_eq!(fork.last_activity, ago(5));
    assert_eq!(
        fork.as_agent().and_then(|card| card.estimated_active_secs),
        Some(17)
    );
}

#[test]
fn waiting_primary_keeps_its_own_clock() {
    let mut primary = agent("codex", "primary", AgentStatus::Waiting, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    primary.registered_at = Some(ago(600));
    primary.estimated_active_secs = Some(10);

    let mut fork = agent("codex", "fork", AgentStatus::Success, 2_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    fork.registered_at = Some(ago(60));
    fork.estimated_active_secs = Some(7);

    let snapshot =
        room(vec![primary, fork]).with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);
    let primary = row(&snapshot, "primary");

    assert_eq!(primary.status(), Some(AgentStatus::Waiting));
    assert_eq!(primary.last_activity, ago(120));
    assert_eq!(
        primary
            .as_agent()
            .and_then(|card| card.estimated_active_secs),
        Some(10)
    );
}

#[test]
fn different_pane_root_does_not_fold_onto_the_primary() {
    let mut primary = agent("codex", "primary", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    primary.registered_at = Some(ago(600));
    primary.estimated_active_secs = Some(10);

    let mut other = agent("codex", "other", AgentStatus::Success, 2_000)
        .worktree("/repo/main")
        .in_pane("%2")
        .active_ago(5);
    other.registered_at = Some(ago(60));
    other.estimated_active_secs = Some(7);

    let snapshot = room(vec![primary, other]).with_live_panes(
        vec![
            pane("%1", "codex", "/repo/main"),
            pane("%2", "codex", "/repo/main"),
        ],
        None,
    );
    let primary = row(&snapshot, "primary");

    assert_eq!(primary.last_activity, ago(120));
    assert_eq!(
        primary
            .as_agent()
            .and_then(|card| card.estimated_active_secs),
        Some(10)
    );
}
