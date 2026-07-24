use super::*;

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
