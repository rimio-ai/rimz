use std::path::{Path, PathBuf};

use super::super::panes::pane_ref_from_id;
use super::super::project::reduce_agent_states;
use super::*;
use crate::agents::SpendWindow;
use crate::feed::FeedKind;
use crate::feed::{RuntimeOwner, RuntimeOwnerKind};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::ledger::snapshot::testkit::*;

#[test]
fn build_groups_by_surface_and_status() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut native = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "n",
        "claude",
        "agent-hook",
    );
    let bridge = FeedItem::new(
        workspace.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "b",
        "rimz",
        "cli",
    );
    let mut answered = FeedItem::new(
        workspace.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "a",
        "rimz",
        "cli",
    );
    answered.status = FeedStatus::Resolved;
    let mut timed = FeedItem::new(
        workspace,
        Surface::Bridge,
        FeedKind::Permission,
        "t",
        "rimz",
        "cli",
    );
    timed.status = FeedStatus::TimedOut;
    native.updated_at += std::time::Duration::from_secs(1);

    let snap = SidebarSnapshot::build(
        WorkspaceId::from_project_root(Path::new("/tmp/x")),
        vec![native, bridge, answered, timed],
        Vec::new(),
    );
    // Pending native + bridge asks surface as attention/working; the
    // resolved and timed-out items are history, so they are dropped — they
    // never become rows.
    assert_eq!(snap.needs_attention.len(), 1);
    assert_eq!(snap.resolver_working.len(), 1);
    assert_eq!(snap.worktree_groups.len(), 1);
    assert_eq!(snap.worktree_groups[0].kind, SidebarWorktreeKind::Workspace);
    assert_eq!(snap.worktree_groups[0].label, "external");
    assert_eq!(snap.worktree_groups[0].rows.len(), 2);
}

#[test]
fn activity_heartbeat_updates_last_activity_not_phase() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut agent = agent("claude", "sess-1", AgentStatus::Running, 50_000);
    agent.phase = TurnPhase::Reasoning;
    let original_seen = agent.last_seen;
    let at = original_seen + std::time::Duration::from_secs(10);
    let touch = AgentActivity {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        at,
    };
    let snap = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![agent])
        .with_agent_activity(&[touch]);

    // The heartbeat is latency, not a lifecycle signal — it advances
    // `last_activity` only, never the turn-phase head.
    assert_eq!(snap.agents[0].phase, TurnPhase::Reasoning);
    assert_eq!(snap.agents[0].last_activity, at);
    assert_eq!(snap.agents[0].last_seen, original_seen);
}

#[test]
fn provider_panel_spending_is_attached_and_ranks_panels() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);
    let codex = agent("codex", "x1", AgentStatus::Idle, 20);
    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![claude, codex]);

    let today_tally = |usd: f64| SpendTally {
        today: SpendWindow {
            usd,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert("claude".to_owned(), today_tally(1.0));
    by_provider.insert("codex".to_owned(), today_tally(5.0));

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    // Codex's today spend (5.0) outranks Claude's (1.0), so it sorts first —
    // even though Codex has no live `total_cost_usd`.
    assert_eq!(snapshot.providers[0].kind, "codex");
    assert_eq!(
        snapshot.providers[0].spending.as_ref().unwrap().today.usd,
        5.0
    );
    let claude_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "claude")
        .expect("claude panel present");
    assert_eq!(claude_panel.spending.as_ref().unwrap().today.usd, 1.0);
}

#[test]
fn provider_with_recorded_spend_earns_a_panel_without_a_session() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    // No live agents and no probed accounts — only recorded fleet spend for
    // Claude. Its history alone must still surface a panel, so the dashboard
    // never reads zero for a provider you spent on earlier.
    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), Vec::new());

    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert(
        "claude".to_owned(),
        SpendTally {
            today: SpendWindow {
                usd: 2.0,
                tokens: 100,
                ..Default::default()
            },
            year: SpendWindow {
                usd: 9.0,
                tokens: 900,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    let claude = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "claude")
        .expect("claude panel from recorded spend alone");
    assert_eq!(claude.spending.as_ref().unwrap().year.usd, 9.0);
}

#[test]
fn provider_without_the_rate_limit_capability_drops_stray_windows() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let reading = RateLimitWindow {
        used_percentage: Some(40),
        resets_at: Some(Timestamp::now() + std::time::Duration::from_secs(3_600)),
        duration_mins: Some(300),
    };
    // Pi declares `rate_limit_windows: false`; Claude declares it true. The
    // same stray session reading must paint a budget bar only where the
    // descriptor declares the surface.
    let mut pi = agent("pi", "p1", AgentStatus::Idle, 10);
    pi.context = Some(ctx_with_limits(vec![reading.clone()]));
    let mut claude = agent("claude", "c1", AgentStatus::Idle, 10);
    claude.context = Some(ctx_with_limits(vec![reading]));

    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![pi, claude])
        .with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());

    let panel = |kind: &str| {
        snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == kind)
            .unwrap_or_else(|| panic!("{kind} panel present"))
    };
    assert!(
        panel("pi").windows.is_empty(),
        "pi's declared absence drops the stray reading"
    );
    assert_eq!(panel("claude").windows.len(), 1);
}

#[test]
fn pending_cli_native_items_do_not_become_sidebar_attention() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let item = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Generic,
        "Should I proceed?",
        "rimz",
        "cli",
    );

    let snap = SidebarSnapshot::build(workspace, vec![item], Vec::new());

    assert!(snap.needs_attention.is_empty());
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn pending_script_items_use_worktree_branch_label() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    item.worktree_path = Some("/repo/rimz".to_owned());
    item.worktree_branch = Some("main".to_owned());

    let snap = SidebarSnapshot::build(workspace, vec![item], Vec::new());

    assert_eq!(snap.worktree_groups.len(), 1);
    assert_eq!(snap.worktree_groups[0].label, "main");
    assert_eq!(
        snap.worktree_groups[0].rows[0].task.as_deref(),
        Some("Should I proceed?")
    );
}

#[test]
fn multiple_pending_asks_for_one_session_render_one_row() {
    // The live pile-up: a session held several pending native_ui asks, and
    // the no-panes rollup emitted one row each. Read-time dedup collapses
    // them to a single row keyed by `(source, agent_id)`.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "sess-1", AgentStatus::Idle, 1_000);
    session.worktree_path = Some("/repo/main".to_owned());

    let mk = |kind: FeedKind| {
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            kind,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        item.worktree_path = Some("/repo/main".to_owned());
        item.payload = serde_json::json!({ "session_id": "sess-1" });
        item
    };

    let items = vec![mk(FeedKind::Permission), mk(FeedKind::Question)];
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, items, Vec::new(), vec![session]);

    let rows = &snapshot.worktree_groups[0].rows;
    let agent_rows: Vec<_> = rows
        .iter()
        .filter(|row| row.row_kind == SidebarRowKind::Agent)
        .collect();
    assert_eq!(
        agent_rows.len(),
        1,
        "two pending asks for one session collapse to one row: {rows:?}"
    );
    assert_eq!(agent_rows[0].status, Some(AgentStatus::Waiting));
}

#[test]
fn agents_on_different_branches_in_one_path_form_two_groups() {
    // Root cause 5: stale rows put two branches under one path, collapsing
    // into a single mislabeled section. Keying on branch splits them into
    // two correctly-labeled groups.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000);
    feature.worktree_path = Some("/repo/shared".to_owned());
    feature.worktree_branch = Some("feature".to_owned());
    let mut main = agent("claude", "sess-b", AgentStatus::Idle, 1_100);
    main.worktree_path = Some("/repo/shared".to_owned());
    main.worktree_branch = Some("main".to_owned());

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![feature, main],
    );

    assert_eq!(
        snapshot.worktree_groups.len(),
        2,
        "two branches under one path split into two groups"
    );
    for group in &snapshot.worktree_groups {
        assert_eq!(group.rows.len(), 1);
        assert_eq!(
            group.rows[0].worktree_branch.as_deref(),
            Some(group.label.as_str()),
            "each group's label matches its branch"
        );
    }
}

#[test]
fn one_branch_path_keeps_agent_and_shell_in_one_group() {
    // The common case must not fragment: a process/shell row carries no
    // branch, so it stays with the single-branch agent in its worktree.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut claude = agent("claude", "sess-a", AgentStatus::Running, 1_000);
    claude.worktree_path = Some("/repo/main".to_owned());
    claude.worktree_branch = Some("main".to_owned());
    claude.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
            .with_live_panes(
                vec![
                    pane("%1", "claude", "/repo/main"),
                    pane("%2", "zsh", "/repo/main"),
                ],
                None,
            );

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "agent and its shell share one worktree group: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "main");
    let rows = &snapshot.worktree_groups[0].rows;
    assert!(rows.iter().any(|row| row.row_kind == SidebarRowKind::Agent));
    assert!(
        rows.iter()
            .any(|row| row.row_kind == SidebarRowKind::Process && row.name == "zsh")
    );
}

#[test]
fn remote_control_host_pane_renders_no_row() {
    // A `claude remote-control` pane (Zellij reports the full command line)
    // is ambient infrastructure: it no longer renders as any row — its
    // presence surfaces as the provider dashboard's `⇅ rc` flag instead.
    // Only the shell pane beside it remains a row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new()).with_live_panes(
        vec![
            pane("%1", "zsh", "/repo/main"),
            pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the shell pane is a row: {rows:?}");
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].name, "zsh");
    assert!(
        rows.iter().all(|row| row.name != "claude"),
        "the host pane must not produce a claude row: {rows:?}",
    );
}

#[test]
fn remote_control_host_pane_filtered_when_detected_by_view_name() {
    // tmux reports only the `claude` basename, but names the window — so the
    // view name marks the host, and that pane is filtered out the same way.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut rc_pane = pane("%2", "claude", "/repo/main");
    rc_pane.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_live_panes(vec![rc_pane], None);

    let rows: Vec<_> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect();
    assert!(
        rows.is_empty(),
        "a host-only pane set produces no rows: {rows:?}",
    );
}

#[test]
fn sub_agent_nests_under_parent_and_never_top_level() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    // Only the parent built a row; the paneless child attaches onto it.
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], Timestamp::now());
    assert_eq!(rows.len(), 1, "the child is never its own top-level row");
    assert_eq!(rows[0].sub_agents.len(), 1);
    assert_eq!(rows[0].sub_agents[0].id, "child-1");
    assert_eq!(rows[0].sub_agents[0].name, "Explore");
}

#[test]
fn orphan_sub_agent_is_dropped() {
    let child = child_state("missing-parent", "child-1", AgentStatus::Running, 5);
    let mut rows: Vec<SidebarRow> = Vec::new();
    attach_sub_agents(&mut rows, &[child], Timestamp::now());
    assert!(rows.is_empty(), "a child with no parent row never renders");
}

#[test]
fn with_subagent_context_folds_onto_child_by_key() {
    use crate::agents::context::SubagentContext;
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    let started = Timestamp::from_second(1_700_000_000).unwrap();
    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, child]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: None,
            description: Some("locate the render seam".to_owned()),
            token_count: Some(12_400),
            started_at: Some(started),
            observed_at: Timestamp::now(),
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
            observed_at: Timestamp::now(),
        },
    };
    let folded = folded.with_subagent_context(vec![absent]);
    assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
}

#[test]
fn with_subagent_context_back_fills_task_from_agent_type() {
    use crate::agents::context::SubagentContext;
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // A fork child: parent_agent_id set, task None (no agent_type in SubagentStart).
    let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
    fork.task = None;
    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, fork]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "fork-1".into(),
        context: SubagentContext {
            agent_type: Some("Explore".to_owned()),
            description: Some("search the ledger".to_owned()),
            token_count: Some(5_000),
            started_at: None,
            observed_at: Timestamp::now(),
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
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // Typed child: task already set by SubagentStart.
    let mut typed = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    typed.task = Some("review".to_owned());
    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, typed]);

    let record = SubagentContextRecord {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "child-1".into(),
        context: SubagentContext {
            agent_type: Some("SomethingElse".to_owned()),
            description: None,
            token_count: None,
            started_at: None,
            observed_at: Timestamp::now(),
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
    let now = Timestamp::from_second(1_700_000_100).unwrap();
    let started = Timestamp::from_second(1_700_000_000).unwrap();

    // Running: elapsed counts to `now` (100s), enrichment projects through.
    let mut running = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    running.subagent_description = Some("locate the render seam".to_owned());
    running.subagent_started_at = Some(started);
    running.total_tokens = Some(12_400);
    let sub = sub_agent_from_state(&running, now);
    assert_eq!(sub.description.as_deref(), Some("locate the render seam"));
    assert_eq!(sub.total_tokens, Some(12_400));
    assert_eq!(sub.elapsed_secs, Some(100));

    // Finished: elapsed freezes at `last_activity` (40s after start), never `now`.
    let mut finished = child_state("sess-root", "child-2", AgentStatus::Success, 0);
    finished.last_activity = Timestamp::from_second(1_700_000_040).unwrap();
    finished.subagent_started_at = Some(started);
    let sub = sub_agent_from_state(&finished, now);
    assert_eq!(sub.elapsed_secs, Some(40));

    // A child with no enrichment (Codex, or pre-first-render) degrades cleanly.
    let bare = child_state("sess-root", "child-3", AgentStatus::Running, 5);
    let sub = sub_agent_from_state(&bare, now);
    assert_eq!(sub.description, None);
    assert_eq!(sub.total_tokens, None);
    assert_eq!(sub.elapsed_secs, None);
}

#[test]
fn finished_sub_agent_drops_once_parent_starts_next_turn() {
    let now = Timestamp::now();
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The current turn began AFTER the child finished — a past-turn child.
    parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 30).unwrap());
    let child = child_state("sess-root", "child-1", AgentStatus::Idle, 60);
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], now);
    assert!(rows[0].sub_agents.is_empty());
}

#[test]
fn running_sub_agent_of_current_turn_is_kept() {
    let now = Timestamp::now();
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The turn began BEFORE the child's activity — live work of this turn.
    parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 90).unwrap());
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 30);
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], now);
    assert_eq!(
        rows[0].sub_agents.len(),
        1,
        "a live child of the current turn is kept"
    );
}

#[test]
fn superseded_running_sub_agent_is_reaped_as_ghost() {
    let now = Timestamp::now();
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The parent moved to a newer turn than the child's last activity: the
    // child never sent `SubagentStop` and is a leftover ghost — reaped so it
    // can't freeze the parent's delegated-wait head.
    parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 30).unwrap());
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 60);
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], now);
    assert!(
        rows[0].sub_agents.is_empty(),
        "a running child from a past turn is a ghost"
    );
}

#[test]
fn finished_sub_agent_of_current_turn_is_kept() {
    let now = Timestamp::now();
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The turn began BEFORE the child finished — same-turn, so it stays.
    parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 90).unwrap());
    let child = child_state("sess-root", "child-1", AgentStatus::Idle, 30);
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], now);
    assert_eq!(rows[0].sub_agents.len(), 1);
}

#[test]
fn sub_agents_sort_by_creation_time_ascending() {
    // Spawn order, not activity, keys the list: the child that started
    // first leads however fresh its siblings' activity is, so the list
    // holds still across refreshes. A child with no reported start time
    // sorts after the dated ones, by id.
    let now = Timestamp::now();
    let started = |secs_ago: i64| Timestamp::from_second(now.as_second() - secs_ago).unwrap();
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The youngest-started child is the most recently active — an
    // activity-keyed sort would lead with it; creation order must not.
    let mut first = child_state("sess-root", "c-late-id", AgentStatus::Idle, 40);
    first.subagent_started_at = Some(started(90));
    let mut second = child_state("sess-root", "c-early-id", AgentStatus::Running, 2);
    second.subagent_started_at = Some(started(60));
    let undated = child_state("sess-root", "c-undated", AgentStatus::Running, 1);
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), undated, second, first], now);
    let ids: Vec<&str> = rows[0].sub_agents.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["c-late-id", "c-early-id", "c-undated"]);
}

#[test]
fn duplicate_children_collapse_to_one_row() {
    // Two reduced states aliasing the same child id must render as one row,
    // so `subagents (N)` never double-counts. Freshest activity wins.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let stale = child_state("sess-root", "child-dup", AgentStatus::Running, 50);
    let fresh = child_state("sess-root", "child-dup", AgentStatus::Running, 5);
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), stale, fresh], Timestamp::now());
    assert_eq!(
        rows[0].sub_agents.len(),
        1,
        "the same child can't appear twice"
    );
    assert_eq!(rows[0].sub_agents[0].id, "child-dup");
}

#[test]
fn typeless_child_renders_degraded_label_never_the_kind() {
    // A child with no type label must not borrow the provider kind, which
    // would render as a phantom `claude` row. This is the "3 Explore + 3
    // claude" regression.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    child.task = None;
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], Timestamp::now());
    let name = &rows[0].sub_agents[0].name;
    assert!(name.starts_with("subagent"), "got {name}");
    assert_ne!(name, "claude");
}

#[test]
fn finished_child_drops_past_ttl_without_a_turn_boundary() {
    // The parent never took a fresh turn (`turn_started_at` stays None), so
    // only the TTL backstop can clear a long-finished child — without it the
    // ghost would linger forever.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Idle,
        SUBAGENT_FINISHED_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent)];
    attach_sub_agents(&mut rows, &[parent.clone(), child], Timestamp::now());
    assert!(
        rows[0].sub_agents.is_empty(),
        "a long-finished child clears on the TTL"
    );
}

#[test]
fn reaper_never_drops_a_subagent() {
    let now = Timestamp::now();
    let parent = agent("claude", "sess-root", AgentStatus::Running, 0);
    // A pidless idle child well past the ghost TTL, plus a same-type sibling
    // that would "supersede" it under the root rule — both survive, because
    // children are exempt and leave only when the parent does.
    let old_child = child_state(
        "sess-root",
        "child-old",
        AgentStatus::Idle,
        GHOST_SESSION_TTL_SECS + 600,
    );
    let new_child = child_state("sess-root", "child-new", AgentStatus::Running, 5);
    assert_eq!(
        reap_survivors(now, vec![parent, old_child, new_child]),
        vec![
            "child-new".to_owned(),
            "child-old".to_owned(),
            "sess-root".to_owned()
        ],
    );
}

#[test]
fn live_panes_add_process_rows_without_attention_counts() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.row_kind, SidebarRowKind::Process);
    assert_eq!(row.name, "zsh");
    assert_eq!(row.status, None);
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

#[test]
fn commandless_unbound_pane_folds_no_row() {
    // A pane whose command is still unknown after carry-forward — mid-birth,
    // or a raced first read — is presence without identity: it folds no row
    // rather than an anonymous `process` under `external`.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_live_panes(vec![raced], None);

    let rows: Vec<_> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect();
    assert!(
        rows.is_empty(),
        "a command-less pane renders no row: {rows:?}"
    );
}

#[test]
fn commandless_pane_with_agent_still_renders_agent_row() {
    // Agent rows bind by stamped pane id, never by command, so a raced read
    // that drops the command never demotes or hides the agent's row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut claude = agent("claude", "sess-a", AgentStatus::Running, 1_000);
    claude.worktree_path = Some("/repo/main".to_owned());
    claude.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    let raced = PaneRef {
        command: None,
        ..pane("%1", "claude", "/repo/main")
    };
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
            .with_live_panes(vec![raced], None);

    let rows: Vec<_> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect();
    assert_eq!(rows.len(), 1, "the stamped agent row survives: {rows:?}");
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
}

#[test]
fn commandless_pane_does_not_form_empty_external_group() {
    // The raced read that drops a command usually drops the cwd too; the
    // filtered pane must not mint a stray `external` header on its way out.
    let root = "/repo/rimz";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%2", "x", "")
    };
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root), raced], None);

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "no external group for the filtered pane: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "rimz");
}

#[test]
fn commandless_pane_keeps_known_process_rows() {
    // The guard is per-pane: a sibling whose command read succeeded keeps
    // its named process row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let raced = PaneRef {
        command: None,
        ..pane("%2", "x", "/repo/main")
    };
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main"), raced], None);

    let rows: Vec<_> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect();
    assert_eq!(rows.len(), 1, "only the named pane is a row: {rows:?}");
    assert_eq!(rows[0].name, "zsh");
}

#[test]
fn is_within_compares_path_components() {
    let root = Path::new("/home/marvin");
    assert!(is_within(root, root));
    assert!(is_within(root, Path::new("/home/marvin/")));
    assert!(is_within(root, Path::new("/home/marvin/sub/dir")));
    // A shared string prefix that is not a component boundary is outside.
    assert!(!is_within(root, Path::new("/home/marvinX")));
    assert!(!is_within(root, Path::new("/home/other")));
    assert!(!is_within(root, Path::new("/")));
}

#[test]
fn out_of_project_process_folds_into_external_catch_all() {
    let root = "/home/marvin/workspace/project-rimz/rimz";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Workspace);
    assert_eq!(group.key, "workspace");
    assert_eq!(group.label, "external");
    assert_eq!(group.rows[0].name, "zsh");
}

#[test]
fn in_project_worktree_pane_keeps_its_own_group() {
    let root = "/repo/rimz";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let worktree = "/repo/rimz/.claude/worktrees/featureX";
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", worktree)], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, worktree);
    assert_eq!(group.label, "featureX");
}

#[test]
fn main_checkout_pane_is_in_project() {
    let root = "/repo/rimz";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root)], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.label, "rimz");
}

#[test]
fn component_boundary_pane_is_external() {
    // cwd shares a string prefix with the root but not a component boundary.
    let workspace = WorkspaceId::from_project_root(Path::new("/home/marvin"));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from("/home/marvin")))
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvinX/repo")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Workspace);
    assert_eq!(group.label, "external");
}

#[test]
fn external_worktree_pane_gets_its_own_pod() {
    // A worktree parked outside the project root — captured by `git worktree
    // list` — is project-related and earns its own pod, not the `external`
    // catch-all the `project_root` prefix test alone would give it.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", external)], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, external);
    assert_eq!(group.label, "feature-wt");
}

#[test]
fn external_worktree_subdir_stays_with_its_worktree() {
    // A cwd nested under an external worktree root is still that worktree's,
    // never `external`.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", "/elsewhere/feature-wt/src")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
}

#[test]
fn non_worktree_path_is_the_only_external() {
    // With the worktree set known, a cwd that is neither under the project
    // root nor inside any worktree (a home shell) is all that's left as
    // `external`.
    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let workspace = WorkspaceId::from_project_root(Path::new(root));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Workspace);
    assert_eq!(group.label, "external");
}

#[test]
fn no_project_root_preserves_per_path_grouping() {
    // With no known root, an outside cwd still gets its own worktree group —
    // the prior behavior, preserved as the safe default.
    let workspace = WorkspaceId::from_project_root(Path::new("/repo/rimz"));
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, "/home/marvin");
    assert_eq!(group.label, "marvin");
}

#[test]
fn live_panes_overlay_matching_agent_rows() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000);
    codex.worktree_path = Some("/repo/main".to_owned());
    codex.worktree_branch = Some("main".to_owned());
    codex.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex])
            .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(snapshot.worktree_groups[0].rows.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.row_kind, SidebarRowKind::Agent);
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_panes_do_not_render_unmatched_ledger_agents() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000);
    codex.worktree_path = Some("/repo/main".to_owned());

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex])
            .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .all(|row| row.row_kind != SidebarRowKind::Agent),
        "non-attention agent rows must come from live pane presence"
    );
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.row_kind == SidebarRowKind::Process && row.name == "zsh"),
        "the live shell pane remains a process row"
    );
}

#[test]
fn live_panes_suppress_stale_agent_attention_without_process() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": "stale-claude" });

    let snapshot = SidebarSnapshot::build(workspace, vec![item], Vec::new()).with_live_panes(
        vec![
            pane(
                "%0",
                "/home/me/.cargo/bin/rimz-sidebar serve --workspace-id ws_x",
                "/repo/main",
            ),
            pane("%1", "zsh", "/repo/main"),
        ],
        None,
    );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .all(|row| row.row_kind == SidebarRowKind::Process && row.name == "zsh"),
        "a stale agent prompt must not claim the sidebar pane or outlive its agent process: {:?}",
        snapshot.worktree_groups[0].rows,
    );
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

#[test]
fn live_panes_keep_agent_attention_with_process() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": "live-claude" });
    // The ask's session is live in the rollup, so it binds to that
    // session's pane and renders as attention.
    let mut session = agent("claude", "live-claude", AgentStatus::Idle, 1_000);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    // The pane runs under a `node` wrapper, not a `claude` foreground — the
    // bind is by the session's stamped pane id, so the command is moot.
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, vec![item], Vec::new(), vec![session])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.row_kind, SidebarRowKind::Agent);
    assert_eq!(row.name, "claude");
    assert_eq!(row.status, Some(AgentStatus::Waiting));
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn newer_subagent_does_not_expire_parent_attention() {
    // A child shares the parent's pane and worktree, so it can be newer than
    // the parent without superseding the parent's human decision surface.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": "parent-claude" });

    let mut parent = agent("claude", "parent-claude", AgentStatus::Running, 1_000);
    parent.worktree_path = Some("/repo/main".to_owned());
    parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    let mut child = agent("claude", "child-claude", AgentStatus::Idle, 2_000);
    child.parent_agent_id = Some("parent-claude".into());
    child.worktree_path = Some("/repo/main".to_owned());
    child.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        vec![item.clone()],
        Vec::new(),
        vec![parent, child],
    )
    .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(
        snapshot.needs_attention[0].request_id, item.request_id,
        "the child must not make the parent's ask stale"
    );
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.id, "parent-claude");
    assert_eq!(row.status, Some(AgentStatus::Waiting));
    assert_eq!(row.request_id, Some(item.request_id));
}

#[test]
fn answered_native_ui_ask_returns_to_running() {
    // The live bug: a native_ui ask is answered in the agent's own UI and
    // the agent keeps working the same turn. The ask stays pending in the
    // ledger, but the activity heartbeat has advanced `last_activity` past
    // the ask, so the row must read `running`, not stay folded to `waiting`.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Question,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": "live-claude" });
    // Ask raised at t=1000.
    item.updated_at = Timestamp::from_second(1_000).unwrap();

    // The agent recorded progress at t=2000 — after the ask — so it has
    // un-blocked and moved on.
    let mut session = agent("claude", "live-claude", AgentStatus::Running, 2_000);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, vec![item], Vec::new(), vec![session])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.row_kind, SidebarRowKind::Agent);
    assert_eq!(
        row.status,
        Some(AgentStatus::Running),
        "an answered ask the agent moved past must not pin the row to waiting"
    );
}

#[test]
fn answered_native_ui_ask_returns_to_running_without_panes() {
    // The same recovery as the pane path, but on the ledger-rollup fallback
    // (`rimz sidebar snapshot` with no live mux). The moved-past guard must
    // apply here too, or the answered ask falsely pins the row to waiting.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Question,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": "live-claude" });
    // Ask raised long ago; the agent recorded progress since (recent
    // `last_activity` via the `agent` helper), so it has moved past it.
    item.updated_at = Timestamp::from_second(1_000).unwrap();
    let mut session = agent("claude", "live-claude", AgentStatus::Running, 2_000);
    session.worktree_path = Some("/repo/main".to_owned());

    // No `with_live_panes`: the snapshot stays on the ledger-rollup path.
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, vec![item], Vec::new(), vec![session]);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::Running),
        "the moved-past recovery must also apply on the no-pane ledger fallback"
    );
}

#[test]
fn stalled_running_agent_recovers_when_activity_resumes() {
    // The stall escalation is self-healing: once the agent's next completed
    // tool touches the activity heartbeat, the fold readvances
    // `last_activity`, `is_stalled` goes false, and the row drops back out
    // of attention with no human action.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    // Silent past the stall window.
    session.last_activity = Timestamp::now()
        - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);

    // A fresh heartbeat lands (the agent's next tool completed).
    let touch = AgentActivity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "live-claude".into(),
        at: Timestamp::now(),
    };
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
            .with_agent_activity(&[touch])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::Running),
        "a fresh heartbeat readvances last_activity, so the stalled row recovers"
    );
}

#[test]
fn stalled_running_agent_escalates_to_attention() {
    // A running agent that records no activity past the stall window is
    // likely wedged; the displayed row escalates to the attention bucket
    // (`!`) and the rollup keeps the true `running` status.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    session.last_activity = Timestamp::now()
        - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::Failed),
        "a long-silent running agent escalates to the attention bucket"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the stalled agent counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the true running status; only the display row escalates"
    );
}

fn ctx_with_limits(windows: Vec<RateLimitWindow>) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: Some(crate::agents::AgentRateLimits { windows }),
        pr: None,
        account: None,
        turn_error: None,
        observed_at: Timestamp::now(),
    }
}

#[test]
fn spent_account_parks_every_resting_agent_of_the_kind() {
    // Account-scoped: one claude session reports a spent 5-hour window, so
    // the whole kind is rate-limited — including a *fresh* idle session that
    // carries no context of its own yet (the "launched into a spent account"
    // case). A running session with a spent account also parks: the budget is
    // gone regardless of whether a turn is nominally in progress.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut reporter = agent("claude", "sess-spent", AgentStatus::Success, 1_000);
    reporter.worktree_path = Some("/repo/main".to_owned());
    reporter.context = Some(ctx_with_limits(vec![window(100, 3_600)]));
    let mut fresh = agent("claude", "sess-fresh", AgentStatus::Idle, 1_100);
    fresh.worktree_path = Some("/repo/main".to_owned());
    let mut working = agent("claude", "sess-busy", AgentStatus::Running, 1_200);
    working.worktree_path = Some("/repo/main".to_owned());

    let snapshot =
        SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![reporter, fresh, working]);
    let status_of = |id: &str| {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("row {id} present"))
            .status
    };
    assert_eq!(status_of("sess-spent"), Some(AgentStatus::RateLimited));
    assert_eq!(
        status_of("sess-fresh"),
        Some(AgentStatus::RateLimited),
        "a fresh idle session inherits the account verdict"
    );
    assert_eq!(
        status_of("sess-busy"),
        Some(AgentStatus::RateLimited),
        "a running session in a spent account parks — the budget is gone regardless"
    );
    // The rollup keeps the true lifecycle status; only the display projects.
    assert_eq!(
        snapshot
            .agents
            .iter()
            .find(|a| a.agent_id == "sess-fresh")
            .unwrap()
            .status,
        AgentStatus::Idle
    );
}

#[test]
fn running_agent_in_spent_account_parks_not_fails() {
    // A running agent that went silent past the stall window AND whose account
    // is spent should surface as RateLimited, not Failed. The rate-limit check
    // takes priority over the stall check so the user sees the real cause.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut stalled = agent("claude", "stalled-spent", AgentStatus::Running, 0);
    stalled.worktree_path = Some("/repo/main".to_owned());
    stalled.context = Some(ctx_with_limits(vec![window(100, 3_600)]));
    stalled.last_activity = Timestamp::now()
        - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);

    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![stalled]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status,
        Some(AgentStatus::RateLimited),
        "rate-limit outranks stall: agent is paused by the account, not wedged"
    );
}

#[test]
fn rate_limit_outranks_the_turn_death_marker() {
    // A rate-limited turn dies on a provider API error (`isApiErrorMessage`)
    // with no `Stop` hook, so the next statusline push delivers the
    // turn-death marker and the spent window *together*. The park wins
    // while the window is spent — the agent is paused by the account, not
    // dead — and the row carries no failure label. Once the window resets,
    // the still-standing marker escalates an agent that failed to resume.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "limited-dead", AgentStatus::Running, 0);
    session.worktree_path = Some("/repo/main".to_owned());
    session.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
    let mut context = ctx_with_limits(vec![window(100, 3_600)]);
    context.turn_error = Some(crate::agents::AgentTurnError {
        at: Timestamp::now() - std::time::Duration::from_secs(10),
        label: Some("You've hit your usage limit".to_owned()),
    });
    session.context = Some(context);

    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![session]);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::RateLimited),
        "rate-limit outranks turn-death: the marker is the limit's own corpse"
    );
    assert!(
        row.turn_error_label.is_none(),
        "a parked row carries no failure label"
    );
}

#[test]
fn running_parent_with_live_child_in_spent_account_parks() {
    // Children share the parent's spent account: a window that tips
    // mid-delegation freezes the child with no `SubagentStop` to come, so
    // the delegated-wait exemption must not hold the parent at `running`
    // forever. The park outranks the exemption.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut parent = agent("claude", "root", AgentStatus::Running, 1_000);
    parent.worktree_path = Some("/repo/main".to_owned());
    parent.context = Some(ctx_with_limits(vec![window(100, 3_600)]));
    let mut child = child_state("root", "child-1", AgentStatus::Running, 5);
    child.kind = AgentKind::new_unchecked("claude");

    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, child]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status,
        Some(AgentStatus::RateLimited),
        "a spent account parks the delegating parent — its children share the budget"
    );
}

#[test]
fn a_window_spent_but_already_reset_does_not_park() {
    // A spent reading whose reset has passed is stale, not limiting — the
    // budget has refilled, so a resting agent reads idle, not parked.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut idle = agent("claude", "sess-1", AgentStatus::Idle, 1_000);
    idle.worktree_path = Some("/repo/main".to_owned());
    idle.context = Some(ctx_with_limits(vec![window(100, -60)]));

    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![idle]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status,
        Some(AgentStatus::Idle),
        "a passed reset means the budget refilled — not rate-limited"
    );
}

#[test]
fn running_parent_with_a_live_subagent_waits_instead_of_stalling() {
    // A running parent that has delegated to a live child shows no heartbeat
    // of its own, so the stall window would falsely escalate it. The
    // delegated-wait exemption keeps it `running` while a child runs; the
    // renderer paints the waiting-on-subagents head from `sub_agents`.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut parent = agent("claude", "root", AgentStatus::Running, 1_000);
    parent.worktree_path = Some("/repo/main".to_owned());
    parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    // Silent past the stall window — its heartbeat is quiet because the work
    // is the child's, not a wedge.
    parent.last_activity = Timestamp::now()
        - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);
    let mut child = child_state("root", "child-1", AgentStatus::Running, 5);
    child.kind = AgentKind::new_unchecked("claude");

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![parent, child],
    )
    .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::Running),
        "a parent delegating to a live child is waiting on it, not stalled"
    );
    assert!(
        row.sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running),
        "the live child is nested so the renderer can paint the wait head"
    );
}

fn ctx_with_turn_error(at: Timestamp, label: &str) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_error: Some(crate::agents::AgentTurnError {
            at,
            label: Some(label.to_owned()),
        }),
        observed_at: Timestamp::now(),
    }
}

#[test]
fn api_error_turn_escalates_running_to_attention() {
    // A turn that died on a provider API error fires no Stop hook, so the
    // rollup keeps `running` — but the transcript marker postdates the
    // agent's own activity, and the projection escalates at once. The
    // headline: the agent is *inside* the stall window (silent only a
    // minute), so this beats the 10-minute backstop.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    session.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
    session.context = Some(ctx_with_turn_error(
        Timestamp::now() - std::time::Duration::from_secs(10),
        "API Error: Overloaded",
    ));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::Failed),
        "the explicit death certificate escalates without waiting out the stall window"
    );
    assert_eq!(
        row.turn_error_label.as_deref(),
        Some("API Error: Overloaded"),
        "the row carries the upstream error text for the card's line 2"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the dead turn counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row escalates"
    );
}

#[test]
fn api_error_self_clears_when_activity_resumes() {
    // Any newer hook event (a prompt, a resume, a rewind) advances
    // `last_activity` past the stale marker and the escalation drops with
    // no human action — the self-clear guard.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    session.last_activity = Timestamp::now() - std::time::Duration::from_secs(30);
    session.context = Some(ctx_with_turn_error(
        Timestamp::now() - std::time::Duration::from_secs(120),
        "API Error: Overloaded",
    ));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status,
        Some(AgentStatus::Running),
        "activity newer than the marker means the session moved on"
    );
    assert!(
        row.turn_error_label.is_none(),
        "a cleared escalation leaves no stale reason label"
    );
}

#[test]
fn api_error_does_not_override_waiting() {
    // A human-blocked ask outranks every derived state, the dead-turn
    // escalation included.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut session = agent("claude", "live-claude", AgentStatus::Waiting, 0);
    session.worktree_path = Some("/repo/main".to_owned());
    session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    session.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
    session.context = Some(ctx_with_turn_error(
        Timestamp::now() - std::time::Duration::from_secs(10),
        "API Error: Overloaded",
    ));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
            .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.status, Some(AgentStatus::Waiting));
    assert!(row.turn_error_label.is_none());
}

#[test]
fn dead_parent_with_live_child_keeps_running() {
    // The delegated-wait exemption wins: a live child's heartbeats are the
    // parent's work, so a stale parent marker never escalates over it. If
    // the children also die, the stall window remains the backstop.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut parent = agent("claude", "root", AgentStatus::Running, 1_000);
    parent.worktree_path = Some("/repo/main".to_owned());
    parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    parent.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
    parent.context = Some(ctx_with_turn_error(
        Timestamp::now() - std::time::Duration::from_secs(10),
        "API Error: Overloaded",
    ));
    let mut child = child_state("root", "child-1", AgentStatus::Running, 5);
    child.kind = AgentKind::new_unchecked("claude");

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![parent, child],
    )
    .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.status, Some(AgentStatus::Running));
    assert!(row.turn_error_label.is_none());
}

#[test]
fn compacting_marker_lights_the_head_then_expires() {
    // A fresh compaction marker pulses the head; one older than the window
    // has expired (the crash backstop), so the head returns to its base.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut fresh = agent("claude", "compacting-now", AgentStatus::Running, 1_000);
    fresh.worktree_path = Some("/repo/main".to_owned());
    fresh.compacting_since = Some(Timestamp::now());
    let mut stale = agent("claude", "compacted-long-ago", AgentStatus::Idle, 1_100);
    stale.worktree_path = Some("/repo/main".to_owned());
    stale.compacting_since = Some(
        Timestamp::now()
            - std::time::Duration::from_secs(crate::feed::COMPACTING_WINDOW_SECS as u64 + 10),
    );

    let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![fresh, stale]);
    let row = |id: &str| {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("row {id} present"))
    };
    assert!(row("compacting-now").compacting, "a fresh marker pulses");
    assert!(
        !row("compacted-long-ago").compacting,
        "a marker past the window has expired"
    );
}

#[test]
fn compaction_event_stamps_then_a_later_event_clears_the_marker() {
    // The reducer treats a `compacting` event as a transient: it stamps
    // `compacting_since` and keeps the prior status (not a transition); the
    // next lifecycle event means compaction is done and clears the marker.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_started" },
    }));
    let compact = lifecycle(serde_json::json!({
        "event_name": "PreCompact",
        "agent_id": "sess-1",
        "signal": { "signal": "compacting" },
    }));
    let after_compact = reduce_agent_states(&[prompt.clone(), compact.clone()]);
    assert!(
        after_compact[0].compacting_since.is_some(),
        "the compaction marker is stamped"
    );
    assert_eq!(
        after_compact[0].status,
        AgentStatus::Running,
        "compaction keeps the prior status — it is not a transition"
    );

    let stop = lifecycle(serde_json::json!({
        "event_name": "Stop",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
    }));
    let after_stop = reduce_agent_states(&[prompt, compact, stop]);
    assert!(
        after_stop[0].compacting_since.is_none(),
        "the next lifecycle event clears the marker"
    );
    assert_eq!(after_stop[0].status, AgentStatus::Success);
}

#[test]
fn two_same_kind_agents_bind_to_their_stamped_panes() {
    // Two claude sessions in one worktree are indistinguishable by name and
    // cwd alone; binding is by the hook-stamped pane id, so each session
    // lands on exactly its own pane instead of cross-wiring the rows.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut older = agent("claude", "sess-a", AgentStatus::Idle, 1_000);
    older.worktree_path = Some("/repo/main".to_owned());
    older.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    let mut newer = agent("claude", "sess-b", AgentStatus::Running, 2_000);
    newer.worktree_path = Some("/repo/main".to_owned());
    newer.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%2")));

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![older, newer],
    )
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
        ],
        None,
    );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    let by_id = |id: &str| {
        rows.iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("row {id} missing from {rows:?}"))
    };
    assert_eq!(by_id("sess-a").pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(by_id("sess-b").pane.as_ref().unwrap().pane_id.raw(), "%2");
}

#[test]
fn agent_binds_only_by_stamped_pane_id() {
    // The pane-keyed invariant: an agent stamped `%2`, but only `%1` is
    // live. `%1`'s command and cwd both match the agent — under the old
    // command/cwd fallback it would have bound. Stamped-id binding refuses
    // it, so `%1` stays a process row and the agent simply does not render.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut claude = agent("claude", "sess-1", AgentStatus::Running, 1_000);
    claude.worktree_path = Some("/repo/main".to_owned());
    claude.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%2")));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
            .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn subagent_never_steals_its_parents_pane() {
    // A subagent runs in its parent's pane, so its lifecycle hooks stamp the
    // parent's pane id — parent and child both claim `%1`. The child here is
    // strictly more recently active than the parked parent, which would let
    // `max_by_key(last_activity)` bind the pane to the child. Panes bind root
    // agents only: `%1` stays the parent's row and the child nests under it.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 1_000);
    parent.worktree_path = Some("/repo/main".to_owned());
    parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    // Newer activity than the parent (5s ago vs ~99s ago) — the flip trigger.
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    child.worktree_path = Some("/repo/main".to_owned());
    child.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![parent, child],
    )
    .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "one pane binds exactly one top-level row");
    assert_eq!(
        rows[0].id, "sess-root",
        "the pane binds the root, not the child"
    );
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(
        rows[0].sub_agents.len(),
        1,
        "the child nests under the parent"
    );
    assert_eq!(rows[0].sub_agents[0].id, "child-1");
    assert_eq!(rows[0].sub_agents[0].name, "Explore");
}

#[test]
fn each_live_pane_yields_exactly_one_row() {
    // One pane = one row, by construction: every live pane produces exactly
    // one row — agent or process — and no pane id is ever duplicated.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let stamped = |id, raw| {
        let mut a = agent("claude", id, AgentStatus::Running, 1_000);
        a.worktree_path = Some("/repo/main".to_owned());
        a.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, raw)));
        a
    };

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![stamped("sess-a", "%1"), stamped("sess-b", "%2")],
    )
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
            pane("%3", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows: Vec<_> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect();
    assert_eq!(rows.len(), 3, "three panes render three rows: {rows:?}");
    let mut pane_ids: Vec<&str> = rows
        .iter()
        .map(|row| row.pane.as_ref().unwrap().pane_id.raw())
        .collect();
    pane_ids.sort_unstable();
    assert_eq!(pane_ids, vec!["%1", "%2", "%3"], "no pane id is duplicated");
    let agents = rows
        .iter()
        .filter(|row| row.row_kind == SidebarRowKind::Agent)
        .count();
    assert_eq!(agents, 2, "the two stamped panes bound their agents");
}

#[test]
fn live_agent_and_process_rows_are_pane_backed() {
    // In a live-pane fold, every visible top-level row is jumpable: agent
    // rows and process rows both carry a pane. A subagent that shares its
    // parent's pane nests in the parent card instead of becoming a second
    // top-level row with the same pane.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut parent = agent("claude", "sess-root", AgentStatus::Running, 1_000);
    parent.worktree_path = Some("/repo/main".to_owned());
    parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 2_000);
    child.worktree_path = Some("/repo/main".to_owned());
    child.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![parent, child],
    )
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows: Vec<_> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect();
    assert_eq!(rows.len(), 2, "root agent + process pane render two rows");
    assert!(
        rows.iter().all(|row| row.pane.is_some()),
        "every visible live-pane row has a pane: {rows:?}",
    );
    assert!(
        rows.iter().all(|row| row.id != "child-1"),
        "the subagent is not a top-level row",
    );
    let parent = rows
        .iter()
        .find(|row| row.id == "sess-root")
        .expect("parent row present");
    assert_eq!(parent.sub_agents.len(), 1);
    assert_eq!(parent.sub_agents[0].id, "child-1");
}

fn paneless_codex(id: &str, worktree: &str, rank: i64) -> AgentState {
    let mut codex = agent("codex", id, AgentStatus::Running, rank);
    // The app-server daemon fires the hook with no mux pane env, so the
    // agent carries its worktree but never stamps a pane.
    codex.worktree_path = Some(worktree.to_owned());
    codex
}

#[test]
fn paneless_codex_agent_binds_to_its_worktree_pane() {
    // The daemon exception: a Codex agent the app-server daemon registered
    // has no stamped pane, but its worktree matches the live `codex` pane's
    // cwd, so the cwd fallback binds it as an agent row — not a process row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn paneless_codex_agent_in_other_worktree_stays_a_process_row() {
    // The cwd fallback never crosses worktrees: a pane-less Codex agent in a
    // different worktree leaves the live `codex` pane a process row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/other", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn paneless_codex_agent_does_not_capture_a_nested_worktree_pane() {
    // Worktree match is exact, not containment: a session checked out at the
    // parent `/repo` must not capture a `codex` pane running in a nested
    // worktree under it (this repo nests worktrees under `.claude/`).
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/sub")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn paneless_codex_does_not_bind_a_non_codex_pane() {
    // The pane's own command gates the fallback: a shell the session dropped
    // back to in the worktree stays a process row, never an agent.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    )
    .with_live_panes(vec![pane("term1", "zsh", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
}

#[test]
fn paneless_claude_agent_is_never_rescued_by_cwd() {
    // Only Codex is daemon-backed and pane-less by construction. A pane-less
    // Claude agent is genuinely gone (Claude always stamps a live pane), so
    // the fallback must leave a matching `claude` pane a process row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut claude = agent("claude", "sess-1", AgentStatus::Running, 1_000);
    claude.worktree_path = Some("/repo/main".to_owned());
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
            .with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
}

#[test]
fn two_paneless_codex_in_one_worktree_bind_most_recent() {
    // When two pane-less Codex sessions claim one worktree — a lingering
    // closed session and a live one — the most-recently-active binds the
    // single live pane; the stale session does not render.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![
            paneless_codex("sess-old", "/repo/main", 1_000),
            paneless_codex("sess-new", "/repo/main", 2_000),
        ],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
    assert_eq!(rows[0].id, "sess-new");
}

#[test]
fn paneless_codex_predating_pane_start_does_not_bind() {
    // The defensive guard on the cwd fallback: when the backend reports the
    // pane's process start, a pane-less Codex session whose last activity
    // predates it belongs to an older instance that once ran in this worktree,
    // not the process now in the pane. A daemon-mode session records the shared
    // daemon pid, so process liveness can't tell the stale one from the live
    // one — so the bind is refused and the fresh pane stays a process row until
    // its own session reports.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let pane_start = Timestamp::now();
    let mut stale = paneless_codex("sess-old", "/repo/main", 1_000);
    stale.last_activity = pane_start - std::time::Duration::from_secs(60);
    let fresh_pane = PaneRef {
        pane_process_start: Some(pane_start),
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![stale])
            .with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].row_kind,
        SidebarRowKind::Process,
        "a session predating the pane start must not bind it",
    );
}

#[test]
fn paneless_codex_active_after_pane_start_binds() {
    // The guard never over-blocks: a session whose last activity is at or after
    // the pane's process start is the live occupant and binds normally.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let pane_start = Timestamp::now();
    let mut live = paneless_codex("sess-1", "/repo/main", 1_000);
    live.last_activity = pane_start + std::time::Duration::from_secs(5);
    let started_pane = PaneRef {
        pane_process_start: Some(pane_start),
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![live])
            .with_live_panes(vec![started_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn fresh_codex_pane_with_proc_start_shows_idle_not_ghost() {
    // The ghost-stats regression. A completed daemon-mode Codex session lingers
    // in the rollup — its owner is the shared, still-alive app-server daemon, so
    // process liveness can never reap it, and the daemon still holds the thread
    // loaded so the loaded-set reap keeps it too. A fresh `codex` then starts in
    // the same worktree. On Zellij the backend reports no pane process start, so
    // the producer stamps the in-pane CLI's `/proc` start; fed that, the guard
    // refuses the stale session and the wired pane renders the synthesized idle
    // row (`○ codex`) — not yesterday's `success` stats — until its own first
    // turn binds a new session.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let pane_start = Timestamp::now();
    let mut ghost = paneless_codex("sess-old", "/repo/main", 1_000);
    ghost.status = AgentStatus::Success;
    ghost.total_tokens = Some(126_621);
    ghost.model = Some("gpt-5.5".to_owned());
    ghost.last_activity = pane_start - std::time::Duration::from_secs(12 * 60 * 60);
    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![ghost]);
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let fresh_pane = PaneRef {
        pane_process_start: Some(pane_start),
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = snapshot.with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
    assert_eq!(rows[0].status, Some(AgentStatus::Idle));
    // The synthesized idle row keys on the pane id, never the stale session, and
    // carries none of its stats.
    assert_eq!(rows[0].id, "tmux:term1");
    assert_ne!(rows[0].id, "sess-old");
    assert_eq!(
        rows[0].total_tokens, None,
        "no ghost tokens on a fresh pane"
    );
    assert_eq!(rows[0].model, None, "no ghost model on a fresh pane");
}

fn daemon_codex(id: &str, worktree: &str, owner_pid: u32) -> AgentState {
    let mut codex = paneless_codex(id, worktree, 1_000);
    codex.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        id,
        owner_pid,
        None,
    ));
    codex.agent_pid = Some(owner_pid);
    codex
}

fn daemon_snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents)
}

fn rollup_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut ids: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn daemon_session_absent_from_loaded_is_reaped() {
    // The shared daemon pid is alive, so process liveness keeps the ghost; the
    // app-server no longer holds the thread, so the loaded-set filter reaps it
    // while keeping the session it still holds.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::from(["t-live".to_owned()]);
    let mut snapshot = daemon_snapshot(vec![
        daemon_codex("t-live", "/repo/a", 7),
        daemon_codex("t-gone", "/repo/b", 7),
    ]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["t-live"]);
}

#[test]
fn unknown_loaded_set_keeps_every_session() {
    // `None` means the daemon was unreachable or its list untrusted — never
    // mass-reap.
    let daemon_pids = BTreeSet::from([7]);
    let mut snapshot = daemon_snapshot(vec![daemon_codex("t-gone", "/repo/b", 7)]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, None);
    assert_eq!(rollup_ids(&snapshot), vec!["t-gone"]);
}

#[test]
fn empty_daemon_pids_keeps_every_session() {
    // No daemon is running, so every session is standalone — process liveness
    // governs them, not the loaded-thread set.
    let loaded = BTreeSet::new();
    let mut snapshot = daemon_snapshot(vec![daemon_codex("t-gone", "/repo/b", 7)]);
    snapshot.drop_dead_daemon_sessions(&BTreeSet::new(), Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["t-gone"]);
}

#[test]
fn standalone_codex_is_not_reaped_by_the_loaded_set() {
    // A session whose owner pid is its own in-pane CLI (not a daemon pid) is not
    // daemon-mode, so its absence from the daemon's loaded set means nothing.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::new();
    let mut snapshot = daemon_snapshot(vec![daemon_codex("t-standalone", "/repo/b", 99)]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["t-standalone"]);
}

#[test]
fn daemon_filter_spares_subagents_and_other_kinds() {
    // A codex subagent id is never a root thread, and a non-codex agent is never
    // daemon-mode — neither is reaped even sharing the daemon pid and absent from
    // the loaded set.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::new();
    let mut sub = daemon_codex("sub-1", "/repo/a", 7);
    sub.parent_agent_id = Some("root-1".into());
    let mut claude = daemon_codex("claude-1", "/repo/c", 7);
    claude.kind = AgentKind::new_unchecked("claude");
    let mut snapshot = daemon_snapshot(vec![sub, claude]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["claude-1", "sub-1"]);
}

#[test]
fn wired_unprompted_codex_pane_renders_as_idle_agent() {
    // Codex registers its session lazily — `SessionStart` rides in with the
    // first prompt — so a launched-but-never-prompted `codex` pane has no
    // agent state. When Codex is wired it must read as an idle agent (`○ codex`
    // with its gauge and a cockpit tally), not a bare, dim process row, the
    // moment it opens.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status, Some(AgentStatus::Idle));
    // No session id exists yet, so the row keys on the pane id (its full
    // mux-qualified form, as `row_from_process` does).
    assert_eq!(rows[0].id, "tmux:term1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    assert_eq!(
        rows[0].model, None,
        "no model until the first turn enriches it"
    );
}

#[test]
fn non_lazy_agent_pane_is_never_idle_synthesized() {
    // The idle-instance synthesis is gated on the agent registering lazily
    // (`Capabilities::registers_lazily`), not merely on being wired. Claude
    // stamps a pane on every session, so an unbound `claude` pane stays a
    // process row even when the producer is told claude is a wired lazy kind —
    // the static descriptor gate refuses it. This is what keeps the lifecycle
    // agent-agnostic (a new lazy agent slots in by declaring the capability)
    // without changing how Claude renders.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["claude".to_owned(), "codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
}

#[test]
fn unwired_codex_pane_stays_a_process_row() {
    // The consent invariant: an unwired Codex can report no status, so its
    // live pane stays a process row (agents are invisible until their hooks
    // are wired). `wired_lazy_kinds` left empty reproduces an un-onboarded
    // Codex.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new())
            .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].name, "codex");
}

#[test]
fn bound_codex_pane_keeps_its_real_agent_over_idle_synthesis() {
    // The idle synthesis is a last resort: a `codex` pane that binds a real
    // (pane-less, cwd-matched) agent keeps that agent's identity and status,
    // never the synthesized idle row — even with Codex wired.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
    assert_eq!(
        rows[0].id, "sess-1",
        "the real agent binds, not a synthesis"
    );
    assert_eq!(rows[0].status, Some(AgentStatus::Running));
}

#[test]
fn two_codex_panes_one_agent_yields_one_real_one_idle() {
    // The multi-codex-per-worktree case: one prompted (pane-less) agent plus a
    // second still-unprompted `codex` pane in the same worktree. The agent
    // binds the first codex pane by cwd; the second synthesizes an idle row —
    // no codex pane is ever left as a process row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(
        vec![
            pane("term1", "codex", "/repo/main"),
            pane("term2", "codex", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.row_kind == SidebarRowKind::Agent),
        "neither codex pane is a process row",
    );
    assert!(
        rows.iter().any(|row| row.id == "sess-1"),
        "the prompted session binds one pane",
    );
    assert!(
        rows.iter().any(|row| row.status == Some(AgentStatus::Idle)),
        "the unprompted pane synthesizes an idle row",
    );
}

#[test]
fn unbound_claude_pane_stays_a_process_row_even_when_codex_wired() {
    // The synthesis is Codex-only: Claude always stamps a live pane, so a
    // `claude` pane with no bound agent is a genuinely-ended session and must
    // read as a process row, never an idle agent — regardless of Codex wiring.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].name, "claude");
}

#[test]
fn stale_session_ask_does_not_render_or_steal_a_pane() {
    // Reproduces the live bug: a pending permission ask whose claude
    // session has ended must not become attention, and must not latch onto
    // a freshly launched codex sharing the worktree.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut stale = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    stale.worktree_path = Some("/repo/main".to_owned());
    stale.payload = serde_json::json!({ "session_id": "ended-claude" });

    // Only a live codex session remains in the rollup.
    let mut codex = agent("codex", "sess-codex", AgentStatus::Idle, 2_000);
    codex.worktree_path = Some("/repo/main".to_owned());
    codex.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, vec![stale], Vec::new(), vec![codex])
            .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    assert!(
        snapshot.needs_attention.is_empty(),
        "stale ask is not attention"
    );
    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the live codex renders");
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status, Some(AgentStatus::Idle));
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn superseded_zombie_ask_yields_pane_to_the_fresh_session() {
    // Live reproduction: a pidless `SessionStart`-only claude never ends and
    // never gets reaped, so it lingers in the rollup with an old pending
    // ask. A freshly launched claude shares the worktree. The ask must not
    // render as attention or pin the dead session's "permission" task and
    // stale timestamp onto the live pane — the fresh session binds it idle.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut stale = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    stale.worktree_path = Some("/repo/main".to_owned());
    stale.payload = serde_json::json!({ "session_id": "zombie-claude" });

    let mut zombie = agent("claude", "zombie-claude", AgentStatus::Idle, 1_000);
    zombie.worktree_path = Some("/repo/main".to_owned());
    let mut fresh = agent("claude", "fresh-claude", AgentStatus::Idle, 2_000);
    fresh.worktree_path = Some("/repo/main".to_owned());
    // Only the fresh session stamped the live pane; the zombie holds none.
    fresh.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        vec![stale],
        Vec::new(),
        vec![zombie, fresh],
    )
    .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    assert!(
        snapshot.needs_attention.is_empty(),
        "the superseded session's ask is not attention"
    );
    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the fresh session renders");
    assert_eq!(rows[0].id, "fresh-claude");
    assert_eq!(rows[0].status, Some(AgentStatus::Idle));
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_codex_command_does_not_corroborate_claude_attention() {
    // Live reproduction: an old Claude ask still has a ledger session, but
    // the only live pane in the worktree is `node /usr/bin/codex`. The
    // pane must remain Codex-shaped instead of inheriting Claude's model
    // and stale ask age.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut stale = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    stale.worktree_path = Some("/repo/main".to_owned());
    stale.payload = serde_json::json!({ "session_id": "stale-claude" });

    let mut claude = agent("claude", "stale-claude", AgentStatus::Idle, 1_000);
    claude.worktree_path = Some("/repo/main".to_owned());
    claude.model = Some("claude-opus-4-7".to_owned());

    let snapshot =
        SidebarSnapshot::build_with_carryover(workspace, vec![stale], Vec::new(), vec![claude])
            .with_live_panes(vec![pane("%1", "node /usr/bin/codex", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    assert_eq!(rows[0].name, "codex");
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

/// User's reported scenario: ledger carries a pile of stale claude
/// observations from killed sessions (no SessionEnd ever fired), all
/// claiming the same worktree path. A fresh claude pane lands. The fresh
/// agent must still bind to its pane — stale count does not block live
/// presence.
#[test]
fn live_claude_pane_binds_despite_pile_of_stale_ledger_ghosts() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let stale_a = {
        let mut a = agent("claude", "stale-a", AgentStatus::Idle, 1_000);
        a.worktree_path = Some("/repo/main".to_owned());
        a
    };
    let stale_b = {
        let mut a = agent("claude", "stale-b", AgentStatus::Idle, 1_001);
        a.worktree_path = Some("/repo/main".to_owned());
        a
    };
    let stale_c = {
        let mut a = agent("claude", "stale-c", AgentStatus::Idle, 1_002);
        a.worktree_path = Some("/repo/main".to_owned());
        a
    };
    let live = {
        let mut a = agent("claude", "live", AgentStatus::Running, i64::from(u32::MAX));
        a.worktree_path = Some("/repo/main".to_owned());
        a.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        a
    };

    let snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        vec![stale_a, stale_b, stale_c, live],
    )
    .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    let agent_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.row_kind == SidebarRowKind::Agent)
        .collect();
    assert_eq!(agent_rows.len(), 1, "only the live claude renders");
    assert_eq!(agent_rows[0].id, "live");
}

#[test]
fn pending_attention_survives_without_pane_fold_in() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let item = FeedItem::new(
        workspace.clone(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );

    let snapshot = SidebarSnapshot::build(workspace, vec![item], Vec::new());

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status,
        Some(AgentStatus::Waiting)
    );
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].task.as_deref(),
        Some("approve deploy?")
    );
}

#[test]
fn calm_tail_cap_never_hides_attention_rows() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut agents = (0..8)
        .map(|i| {
            let mut agent = agent(
                "codex",
                &format!("sess-{i}"),
                AgentStatus::Running,
                1_000 + i,
            );
            agent.worktree_path = Some("/repo/main".to_owned());
            agent
        })
        .collect::<Vec<_>>();
    let mut failed = agent("claude", "failed", AgentStatus::Failed, 2_000);
    failed.worktree_path = Some("/repo/main".to_owned());
    agents.push(failed);

    let snapshot = SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.status == Some(AgentStatus::Failed)),
        "attention rows remain visible past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn calm_tail_cap_never_hides_focused_rows() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let agents = (0..8)
        .map(|i| {
            let mut agent = agent(
                "codex",
                &format!("sess-{i}"),
                AgentStatus::Running,
                1_000 + i,
            );
            agent.worktree_path = Some("/repo/main".to_owned());
            if i == 0 {
                agent.pane = Some(PaneRef {
                    pane_id: PaneId::from_parts(MuxName::Tmux, "%99"),
                    session_name: "rimz-test".to_owned(),
                    view_id: Some("@0".to_owned()),
                    view_kind: Some(crate::ids::ViewKind::Window),
                    view_name: None,
                    is_focused: true,
                    command: Some("codex".to_owned()),
                    cwd: Some("/repo/main".to_owned()),
                    pane_pid: None,
                    pane_process_start: None,
                    rss_kb: None,
                    cpu_pct: None,
                    io_bps: None,
                });
            }
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.id == "sess-0"),
        "the focused running pane remains visible even past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn bucket_order_puts_attention_first_and_running_last() {
    // Scrambled input proves the sort, not the insertion order.
    let agents = [
        AgentStatus::Running,
        AgentStatus::Success,
        AgentStatus::Idle,
        AgentStatus::Failed,
        AgentStatus::Waiting,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| agent_in(&format!("sess-{i}"), "/repo/main", status, 1_000 + i as i64))
    .collect::<Vec<_>>();

    let snapshot = SidebarSnapshot::build_with_carryover(
        WorkspaceId::from_project_root(Path::new("/tmp/x")),
        Vec::new(),
        Vec::new(),
        agents,
    );

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.status)
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Failed),
            Some(AgentStatus::Idle),
            Some(AgentStatus::Success),
            Some(AgentStatus::Running),
        ],
        "attention leads; working agents sink to the bottom of the group"
    );
}

#[test]
fn calm_bucket_holds_stable_spawn_order() {
    // Idle agents with distinct spawn times (and one with no pane). The
    // bucket holds spawn order — oldest first — regardless of activity.
    let specs: [(&str, Option<u64>); 4] = [
        ("late", Some(100)),
        ("nopane", None),
        ("early", Some(300)),
        ("mid", Some(200)),
    ];
    let agents = specs
        .into_iter()
        .enumerate()
        .map(|(i, (id, ago_secs))| {
            let mut agent = agent_in(id, "/repo/main", AgentStatus::Idle, 1_000 + i as i64);
            agent.pane = ago_secs.map(|secs| {
                pane_started(
                    &format!("%{i}"),
                    "/repo/main",
                    Timestamp::now() - std::time::Duration::from_secs(secs),
                )
            });
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = SidebarSnapshot::build_with_carryover(
        WorkspaceId::from_project_root(Path::new("/tmp/x")),
        Vec::new(),
        Vec::new(),
        agents,
    );

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Oldest pane first; the paneless row falls to the bucket tail.
    assert_eq!(order, vec!["early", "mid", "late", "nopane"]);
}

#[test]
fn attention_bucket_sorts_longest_overdue_first() {
    // Scrambled input; a higher rank means more recent activity.
    let agents = vec![
        ("wait-new", AgentStatus::Waiting, 9_000),
        ("wait-old", AgentStatus::Waiting, 1_000),
        ("fail-new", AgentStatus::Failed, 8_000),
        ("fail-old", AgentStatus::Failed, 2_000),
    ]
    .into_iter()
    .map(|(id, status, rank)| agent_in(id, "/repo/main", status, rank))
    .collect::<Vec<_>>();

    let snapshot = SidebarSnapshot::build_with_carryover(
        WorkspaceId::from_project_root(Path::new("/tmp/x")),
        Vec::new(),
        Vec::new(),
        agents,
    );

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Waiting leads failed; within each, the longest-overdue (oldest activity) rises.
    assert_eq!(order, vec!["wait-old", "wait-new", "fail-old", "fail-new"]);
}

#[test]
fn group_tiering_floats_attention_and_tails_external() {
    let labels_for = |agents: Vec<AgentState>| {
        SidebarSnapshot::build_with_carryover(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            Vec::new(),
            Vec::new(),
            agents,
        )
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>()
    };
    let external = |id: &str, status: AgentStatus| agent("claude", id, status, 1_000);

    // A calm external sinks below calm project worktrees; an attention
    // worktree leads regardless of its name.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Failed, 1_000),
            agent_in("a2", "/repo/alpha", AgentStatus::Idle, 1_000),
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Idle),
        ]),
        vec!["alpha", "beta", "external"]
    );

    // The external catch-all rises out of the tail only when it holds an
    // attention agent (waiting or failed).
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["external", "beta"]
    );
}

#[test]
fn liveness_drops_dead_agent_pid_and_rebuilds_groups() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000);
    codex.agent_pid = Some(424_242);
    codex.agent_process_start = Some("12345".to_owned());
    codex.worktree_branch = Some("main".to_owned());

    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status,
        Some(AgentStatus::Running)
    );

    snapshot.drop_dead_agents_with(|pid, start| {
        assert_eq!(pid, 424_242);
        assert_eq!(start, Some("12345"));
        false
    });

    assert!(snapshot.agents.is_empty());
    assert!(snapshot.worktree_groups.is_empty());
}

/// Build a single-agent rollup, run the reap, and return the surviving
/// agent ids. Timestamps are stamped relative to `now` so the TTL rules are
/// exercised deterministically.
fn reap_survivors(now: Timestamp, agents: Vec<AgentState>) -> Vec<String> {
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        WorkspaceId::from_project_root(Path::new("/tmp/x")),
        Vec::new(),
        Vec::new(),
        agents,
    );
    snapshot.reap_stale_sessions(now);
    let mut ids: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.to_string())
        .collect();
    ids.sort();
    ids
}

fn aged(mut agent: AgentState, now: Timestamp, secs_ago: i64) -> AgentState {
    let at = Timestamp::from_second(now.as_second() - secs_ago).unwrap();
    agent.last_activity = at;
    agent.last_seen = at;
    agent
}

#[test]
fn reap_drops_pidless_session_past_ttl_but_keeps_recent_and_pidful() {
    let now = Timestamp::now();
    let mut stale = aged(
        agent("claude", "stale", AgentStatus::Idle, 0),
        now,
        GHOST_SESSION_TTL_SECS + 60,
    );
    stale.worktree_path = Some("/repo/stale".to_owned());
    let mut recent = aged(agent("claude", "recent", AgentStatus::Idle, 0), now, 60);
    recent.worktree_path = Some("/repo/recent".to_owned());
    // Old but pid-bearing: TTL reaping is for pidless ghosts only.
    let mut pidful = aged(
        agent("codex", "pidful", AgentStatus::Idle, 0),
        now,
        GHOST_SESSION_TTL_SECS * 10,
    );
    pidful.worktree_path = Some("/repo/pidful".to_owned());
    pidful.agent_pid = Some(4242);

    assert_eq!(
        reap_survivors(now, vec![stale, recent, pidful]),
        vec!["pidful".to_owned(), "recent".to_owned()],
        "only the pidless, past-TTL ghost is reaped"
    );
}

#[test]
fn reap_collapses_superseded_paneless_session_to_the_newest() {
    let now = Timestamp::now();
    let mut older = aged(agent("codex", "older", AgentStatus::Idle, 0), now, 120);
    older.worktree_path = Some("/repo/a".to_owned());
    older.worktree_branch = Some("main".to_owned());
    let mut newer = aged(agent("codex", "newer", AgentStatus::Idle, 0), now, 60);
    newer.worktree_path = Some("/repo/a".to_owned());
    newer.worktree_branch = Some("main".to_owned());

    assert_eq!(
        reap_survivors(now, vec![older, newer]),
        vec!["newer".to_owned()],
        "the older paneless session on the same path+branch is reaped"
    );
}

#[test]
fn reap_keeps_concurrent_agents_each_holding_a_distinct_pane() {
    // The one-pane-one-row safety property: two same-branch agents in
    // distinct panes are both live and must both survive supersession.
    let now = Timestamp::now();
    let mut older = aged(agent("claude", "older", AgentStatus::Running, 0), now, 120);
    older.worktree_path = Some("/repo/a".to_owned());
    older.worktree_branch = Some("main".to_owned());
    older.agent_pid = Some(111);
    older.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
    let mut newer = aged(agent("claude", "newer", AgentStatus::Running, 0), now, 60);
    newer.worktree_path = Some("/repo/a".to_owned());
    newer.worktree_branch = Some("main".to_owned());
    newer.agent_pid = Some(222);
    newer.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%2")));

    assert_eq!(
        reap_survivors(now, vec![older, newer]),
        vec!["newer".to_owned(), "older".to_owned()],
        "an agent holding its own distinct pane is never reaped"
    );
}

fn window(used: u8, resets_in_secs: i64) -> RateLimitWindow {
    let now = Timestamp::now();
    let resets_at = if resets_in_secs >= 0 {
        now + std::time::Duration::from_secs(resets_in_secs as u64)
    } else {
        now - std::time::Duration::from_secs((-resets_in_secs) as u64)
    };
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(resets_at),
        duration_mins: Some(300),
    }
}

#[test]
fn stable_window_ignores_passed_resets_and_keeps_the_most_drained() {
    let now = Timestamp::now();
    // A stale window (reset already passed) reads low; two live windows
    // report 50% and 80%. The stale one is dropped, and the most-drained
    // live survivor (80%) wins — never over-promising remaining budget.
    let live_50 = window(50, 3_600);
    let live_80 = window(80, 1_800);
    let stale_10 = window(10, -60);

    let pick = stable_window(
        [live_50.clone(), live_80.clone(), stale_10.clone()].into_iter(),
        now,
    )
    .expect("a live window survives");
    assert_eq!(pick.used_percentage, Some(80));

    // Order-independent: the producer must not flicker with session order.
    let reversed = stable_window([stale_10, live_80, live_50].into_iter(), now)
        .expect("a live window survives");
    assert_eq!(reversed.used_percentage, Some(80));
}

#[test]
fn stable_window_is_none_when_every_reading_is_stale() {
    let now = Timestamp::now();
    assert!(stable_window([window(90, -10), window(40, -3_600)].into_iter(), now).is_none());
}

#[test]
fn stable_window_falls_back_to_an_undated_reading() {
    // A window with no reset instant can't be aged out; it is the last-resort
    // reading only when nothing with a live reset survives.
    let now = Timestamp::now();
    let undated = RateLimitWindow {
        used_percentage: Some(33),
        resets_at: None,
        duration_mins: Some(300),
    };
    let pick = stable_window([window(90, -10), undated].into_iter(), now)
        .expect("the undated reading backstops the stale one");
    assert_eq!(pick.used_percentage, Some(33));
}

#[test]
fn stable_windows_picks_one_per_duration_sorted_short_to_long() {
    let now = Timestamp::now();
    let mk = |used: u8, mins: u32| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + std::time::Duration::from_secs(3_600)),
        duration_mins: Some(mins),
    };
    // Two sessions, each reporting a 5h and a 30d window at different drains.
    let readings = [mk(10, 43_800), mk(20, 300), mk(40, 43_800), mk(5, 300)];
    let stable = stable_windows(readings.into_iter(), now);
    assert_eq!(stable.len(), 2, "one bar per duration");
    assert_eq!(
        stable[0].duration_mins,
        Some(300),
        "short window sorts first"
    );
    assert_eq!(stable[0].used_percentage, Some(20), "most-drained 5h kept");
    assert_eq!(
        stable[1].duration_mins,
        Some(43_800),
        "long window sorts last"
    );
    assert_eq!(stable[1].used_percentage, Some(40), "most-drained 30d kept");
}
