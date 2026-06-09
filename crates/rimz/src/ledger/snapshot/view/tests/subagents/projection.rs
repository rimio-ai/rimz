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

    // A child with no enrichment (Codex, or pre-first-render) degrades cleanly.
    let bare = child_state("sess-root", "child-3", AgentStatus::Running, 5);
    let sub = sub_agent_from_state(&bare, now);
    assert_eq!(sub.phase, TurnPhase::Idle);
    assert_eq!(sub.description, None);
    assert_eq!(sub.total_tokens, None);
    assert_eq!(sub.elapsed_secs, None);
    assert_eq!(sub.model, None);
    assert_eq!(sub.effort, None);
}

#[test]
fn finished_sub_agent_drops_once_parent_starts_next_turn() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The current turn began AFTER the child finished — a past-turn child.
    parent.turn_started_at = Some(ago(30));
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(rows[0].sub_agents().is_empty());
}

#[test]
fn running_sub_agent_of_current_turn_is_kept() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The turn began BEFORE the child's activity — live work of this turn.
    parent.turn_started_at = Some(ago(90));
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 30);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "a live child of the current turn is kept"
    );
}

#[test]
fn superseded_running_sub_agent_is_reaped_as_ghost() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The parent moved to a newer turn than the child's last activity: the
    // child never sent `SubagentStop` and is a leftover ghost — reaped so it
    // can't freeze the parent's delegated-wait head.
    parent.turn_started_at = Some(ago(30));
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a running child from a past turn is a ghost"
    );
}

#[test]
fn finished_sub_agent_of_current_turn_is_kept() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The turn began BEFORE the child finished — same-turn, so it stays.
    parent.turn_started_at = Some(ago(90));
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 30);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(rows[0].sub_agents().len(), 1);
}

#[test]
fn finished_sub_agent_of_current_turn_survives_ghost_ttl() {
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The parent has a known current turn that began before the child
    // finished. Finished children are scoped by that turn boundary, not by
    // age, so an idle-between-turns parent keeps the verdict past the TTL.
    parent.turn_started_at = Some(ago(GHOST_SESSION_TTL_SECS + 20));
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Success,
        GHOST_SESSION_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "a finished child with a known same-turn boundary survives the ghost TTL"
    );
}
