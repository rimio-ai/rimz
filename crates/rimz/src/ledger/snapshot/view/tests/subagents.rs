use super::*;

// ── Subagent nesting, retention, and enrichment ──────────────────────────────

#[test]
fn sub_agent_nests_under_parent_and_never_top_level() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    // Only the parent built a row; the paneless child attaches onto it.
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(rows.len(), 1, "the child is never its own top-level row");
    assert_eq!(rows[0].sub_agents().len(), 1);
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
}

#[test]
fn orphan_sub_agent_is_dropped() {
    let child = child_state("missing-parent", "child-1", AgentStatus::Running, 5);
    let mut rows: Vec<SidebarRow> = Vec::new();
    attach_sub_agents(&mut rows, &[child], epoch());
    assert!(rows.is_empty(), "a child with no parent row never renders");
}

// ── Child activity folds onto the parent's displayed clock ───────────────────

#[test]
fn child_activity_advances_parent_displayed_clock() {
    // A delegating parent is quiet because the work is its children's: the
    // freshest child activity becomes the row's displayed `last_activity`,
    // while the rollup state keeps the parent's own clock.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100).active_ago(540);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    assert_eq!(row(&snapshot, "sess-root").last_activity, ago(5));
    let rollup = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "sess-root")
        .expect("parent in rollup");
    assert_eq!(
        rollup.last_activity,
        ago(540),
        "the fold is display-only; the rollup keeps the parent's own clock"
    );
}

#[test]
fn recently_finished_child_holds_off_the_stall() {
    // The fold runs before the displayed-status projection, so the stall
    // check reads the folded clock: a parent silent past the stall window
    // whose child finished four minutes ago is alive, not wedged.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100).active_ago(660);
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 240);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let row = row(&snapshot, "sess-root");
    assert_eq!(row.status(), Some(AgentStatus::Running), "not a stall");
    assert_eq!(row.last_activity, ago(240));
}

#[test]
fn waiting_parent_keeps_its_ask_clock() {
    // A `waiting` row's age measures how long the ask has needed a human, so
    // child activity never re-clocks it.
    let parent = agent("claude", "sess-root", AgentStatus::Waiting, 100).active_ago(120);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    assert_eq!(row(&snapshot, "sess-root").last_activity, ago(120));
}

#[test]
fn turn_dead_parent_keeps_the_death_certificate() {
    // A turn that died on a provider API error keeps its own clock: the
    // marker postdates the parent's activity, so the fold abstains and the
    // finished child's fresher activity can never mask the escalation.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100)
        .active_ago(120)
        .turn_error(60, "API Error: Overloaded");
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 5);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let row = row(&snapshot, "sess-root");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "the turn death holds"
    );
    assert_eq!(row.last_activity, ago(120), "the fold abstained");
    assert_eq!(row.turn_error_label(), Some("API Error: Overloaded"));
}

#[test]
fn with_subagent_context_folds_onto_child_by_key() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let started = ago(100);
    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("locate the render seam".to_owned()),
            token_count: Some(12_400),
            started_at: Some(started),
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let child = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child in rollup");
    assert_eq!(
        child.subagent_description.as_deref(),
        Some("locate the render seam")
    );
    assert_eq!(child.total_tokens, Some(12_400));
    assert_eq!(child.subagent_started_at, Some(started));

    // A record whose child is absent from the rollup is dropped — the key it
    // is filed under is authority.
    let absent = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "ghost".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("nowhere".to_owned()),
            token_count: None,
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = folded.with_subagent_context(vec![absent]);
    assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
}

#[test]
fn with_subagent_context_back_fills_task_from_agent_type() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // A fork child: parent_agent_id set, task None (no agent_type in SubagentStart).
    let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
    fork.task = None;
    let snapshot = room(Vec::new(), vec![parent, fork]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "fork-1".into(),
        context: SubagentContext {
            agent_type: Some("Explore".to_owned()),
            description: Some("search the ledger".to_owned()),
            token_count: Some(5_000),
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let fork = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "fork-1")
        .expect("fork in rollup");
    assert_eq!(
        fork.task.as_deref(),
        Some("Explore"),
        "agent_type back-fills task"
    );
    assert_eq!(
        fork.subagent_description.as_deref(),
        Some("search the ledger")
    );
}

#[test]
fn with_subagent_context_does_not_overwrite_existing_task() {
    use crate::agents::context::SubagentContext;
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // Typed child: task already set by SubagentStart.
    let mut typed = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    typed.task = Some("review".to_owned());
    let snapshot = room(Vec::new(), vec![parent, typed]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: Some("SomethingElse".to_owned()),
            description: None,
            token_count: None,
            started_at: None,
            observed_at: epoch(),
        },
    };
    let folded = snapshot.with_subagent_context(vec![record]);
    let typed = folded
        .agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child in rollup");
    assert_eq!(
        typed.task.as_deref(),
        Some("review"),
        "lifecycle-established task must not be overwritten by enrichment",
    );
}

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

#[test]
fn sub_agents_sort_by_creation_time_ascending() {
    // Spawn order, not activity, keys the list: the child that started
    // first leads however fresh its siblings' activity is, so the list
    // holds still across refreshes. A child with no reported start time
    // sorts after the dated ones, by id.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The youngest-started child is the most recently active — an
    // activity-keyed sort would lead with it; creation order must not.
    let mut first = child_state("sess-root", "c-late-id", AgentStatus::Idle, 40);
    first.subagent_started_at = Some(ago(90));
    let mut second = child_state("sess-root", "c-early-id", AgentStatus::Running, 2);
    second.subagent_started_at = Some(ago(60));
    let undated = child_state("sess-root", "c-undated", AgentStatus::Running, 1);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(
        &mut rows,
        &[parent.clone(), undated, second, first],
        epoch(),
    );
    let ids: Vec<&str> = rows[0].sub_agents().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["c-late-id", "c-early-id", "c-undated"]);
}

#[test]
fn duplicate_children_collapse_to_one_row() {
    // Two reduced states aliasing the same child id must render as one row,
    // so `subagents (N)` never double-counts. Freshest activity wins.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let stale = child_state("sess-root", "child-dup", AgentStatus::Running, 50);
    let fresh = child_state("sess-root", "child-dup", AgentStatus::Running, 5);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), stale, fresh], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "the same child can't appear twice"
    );
    assert_eq!(rows[0].sub_agents()[0].id, "child-dup");
}

#[test]
fn typeless_child_renders_degraded_label_never_the_kind() {
    // A child with no type label must not borrow the provider kind, which
    // would render as a phantom `claude` row. This is the "3 Explore + 3
    // claude" regression.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    child.task = None;
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    let name = &rows[0].sub_agents()[0].name;
    assert!(name.starts_with("subagent"), "got {name}");
    assert_ne!(name, "claude");
}

#[test]
fn running_child_past_ghost_ttl_is_reaped() {
    // A running child that never sent `SubagentStop` and has been silent past
    // the generous ghost TTL is a leftover — reaped so it can't freeze the
    // parent's delegated-wait head, even with no fresh turn boundary.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Running,
        GHOST_SESSION_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a running child silent past the ghost TTL is reaped"
    );
}

#[test]
fn finished_child_inside_ghost_ttl_is_kept_without_turn_boundary() {
    // The regression: a finished child used to clear on a 5-minute TTL even
    // mid-turn, vanishing from the list while its siblings still ran. A
    // finished child stays through the parent's turn; the ghost TTL is only
    // the age backstop when no turn boundary exists.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 60 * 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "a finished child inside the ghost TTL is kept without a turn boundary"
    );
}

#[test]
fn finished_child_without_turn_boundary_reaped_past_ghost_ttl() {
    // If the parent never recorded a turn boundary, the ghost TTL is the
    // fallback that keeps a finished child from becoming ledger-scoped.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Success,
        GHOST_SESSION_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a finished child silent past the ghost TTL is reaped without a turn boundary"
    );
}

#[test]
fn newer_subagent_does_not_expire_parent_attention() {
    // A child shares the parent's pane and worktree, so it can be newer than
    // the parent without superseding the parent's human decision surface.
    let item = agent_ask(FeedKind::Permission, "claude", "parent-claude");

    let parent = agent("claude", "parent-claude", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let mut child = agent("claude", "child-claude", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");
    child.parent_agent_id = Some("parent-claude".into());

    let snapshot = room(vec![item.clone()], vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(
        snapshot.needs_attention[0].request_id, item.request_id,
        "the child must not make the parent's ask stale"
    );
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.id, "parent-claude");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id().cloned(), Some(item.request_id));
}
