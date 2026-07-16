use super::*;

#[test]
fn sub_agent_projection_carries_enrichment_and_freezes_finished_elapsed() {
    let now = epoch();
    let started = ago(100);

    // Running: elapsed counts to `now` (100s), enrichment projects through.
    let mut running = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    running.phase = TurnPhase::Reasoning;
    running.subagent_description = Some("locate the render seam".to_owned());
    running.subagent_started_at = Some(started);
    running.total_tokens = Some(12_400);
    running.model = Some("claude-opus-4-8".to_owned());
    running.effort = Some("high".to_owned());
    let sub = sub_agent_from_state(&running, now);
    assert_eq!(sub.phase, TurnPhase::Reasoning);
    assert_eq!(sub.description.as_deref(), Some("locate the render seam"));
    assert_eq!(sub.total_tokens, Some(12_400));
    assert_eq!(sub.elapsed_secs, Some(100));
    assert_eq!(sub.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(sub.effort.as_deref(), Some("high"));

    // Finished: elapsed freezes at `last_activity` (40s after start), never `now`.
    let mut finished = child_state("sess-root", "child-2", AgentStatus::Success, 0);
    finished.last_activity = ago(60);
    finished.subagent_started_at = Some(started);
    let sub = sub_agent_from_state(&finished, now);
    assert_eq!(sub.elapsed_secs, Some(40));

    // Codex has no statusline start time, so registration supplies elapsed.
    let mut bare = child_state("sess-root", "child-3", AgentStatus::Running, 5);
    bare.registered_at = Some(ago(5));
    bare.description = Some("adapter task description".to_owned());
    let sub = sub_agent_from_state(&bare, now);
    assert_eq!(sub.phase, TurnPhase::Idle);
    assert_eq!(sub.description.as_deref(), Some("adapter task description"));
    assert_eq!(sub.total_tokens, None);
    assert_eq!(sub.elapsed_secs, Some(5));
    assert_eq!(sub.model, None);
    assert_eq!(sub.effort, None);

    let mut named = child_state("sess-root", "child-4", AgentStatus::Running, 5);
    named.name = Some("Atlas".to_owned());
    named.name_explicit = true;
    named.task = Some("research/explore_hooks".to_owned());
    let sub = sub_agent_from_state(&named, now);
    assert_eq!(sub.name, "Atlas");
    assert_eq!(sub.task.as_deref(), Some("research/explore_hooks"));
}

#[test]
fn live_descendant_projects_clean_resting_parents_to_delegating_running() {
    for status in [
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Running,
    ] {
        let parent = agent("codex", "sess-root", status, 100).worktree("/repo/main");
        let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        child.kind = AgentKind::new_unchecked("codex");
        let snapshot = room_with_agent_panes(vec![parent, child]);
        assert_eq!(
            row(&snapshot, "sess-root").status(),
            Some(AgentStatus::Running)
        );
    }

    let parent = agent("codex", "sess-root", AgentStatus::Success, 100).worktree("/repo/main");
    let mut child = child_state("sess-root", "child-1", AgentStatus::Success, 5);
    child.kind = AgentKind::new_unchecked("codex");
    let snapshot = room_with_agent_panes(vec![parent, child]);
    assert_eq!(
        row(&snapshot, "sess-root").status(),
        Some(AgentStatus::Success),
        "the durable resting state returns after the final child stops"
    );
}

#[test]
fn sub_agent_retention_tracks_the_parent_turn_boundary() {
    for (label, parent_turn_started_secs, child_status, child_secs, expect_kept) in [
        (
            "finished child drops once parent starts next turn",
            30,
            AgentStatus::Success,
            60,
            false,
        ),
        (
            "running child of current turn is kept",
            90,
            AgentStatus::Running,
            30,
            true,
        ),
        (
            "running child from past turn is reaped",
            30,
            AgentStatus::Running,
            60,
            false,
        ),
        (
            "finished child of current turn is kept",
            90,
            AgentStatus::Success,
            30,
            true,
        ),
        (
            "finished child of known same turn survives ghost ttl",
            GHOST_SESSION_TTL_SECS + 20,
            AgentStatus::Success,
            GHOST_SESSION_TTL_SECS + 10,
            true,
        ),
    ] {
        let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        parent.turn_started_at = Some(ago(parent_turn_started_secs));
        let child = child_state("sess-root", "child-1", child_status, child_secs);
        let mut rows = vec![row_from_agent(&parent, epoch())];
        attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
        assert_eq!(!rows[0].sub_agents().is_empty(), expect_kept, "{label}");
    }
}
