use super::*;

fn agent(id: &str) -> AgentState {
    rimz::testkit::agent_state("codex", id, Timestamp::UNIX_EPOCH)
}

fn pane_agent(id: &str, pane: &str) -> PaneAgent {
    PaneAgent {
        root_lane: false,
        kind: AgentKind::new_unchecked("codex"),
        kind_ordinal: None,
        name: Some(format!("{id}-name")),
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(AgentSessionId::from(id)),
        pane_id: PaneId::from_parts(rimz::MuxName::Tmux, pane),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    }
}

#[test]
fn pane_identity_wins_over_launch_identity() {
    let first = agent("first");
    let second = agent("second");
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![first, second],
        Timestamp::UNIX_EPOCH,
    );
    snapshot.agent_panes = vec![pane_agent("first", "%1"), pane_agent("second", "%2")];
    let identity = SelfIdentity {
        pane: Some(PaneId::from_parts(rimz::MuxName::Tmux, "%2")),
        kind: Some(AgentKind::new_unchecked("codex")),
        name: Some("first-name".to_owned()),
        ..SelfIdentity::default()
    };

    assert_eq!(
        identity
            .resolve(&snapshot)
            .as_ref()
            .map(AgentSessionId::as_str),
        Some("second")
    );
}

#[test]
fn launch_identity_matches_uniquely_and_missing_identity_matches_nothing() {
    let mut first = agent("first");
    first.name = Some("first-name".to_owned());
    first.profile = Some("planner".to_owned());
    first.role = Some("lead".to_owned());
    let second = agent("second");
    let snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![first, second],
        Timestamp::UNIX_EPOCH,
    );
    let identity = SelfIdentity {
        kind: Some(AgentKind::new_unchecked("codex")),
        name: Some("first-name".to_owned()),
        profile: Some("planner".to_owned()),
        role: Some("lead".to_owned()),
        ..SelfIdentity::default()
    };

    assert_eq!(
        identity
            .resolve(&snapshot)
            .as_ref()
            .map(AgentSessionId::as_str),
        Some("first")
    );
    assert_eq!(SelfIdentity::default().resolve(&snapshot), None);
}

#[test]
fn delegated_park_reports_paused_without_parent_turn_error() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut parent = agent("parent");
    parent.status = AgentStatus::Running;
    parent.last_activity = now;
    let mut pane =
        rimz::pane::PaneRef::from_id(rimz::PaneId::from_parts(rimz::MuxName::Tmux, "%1"));
    pane.command = Some("codex".to_owned());
    let mut child = agent("child");
    child.status = AgentStatus::Running;
    child.last_activity = now - jiff::SignedDuration::from_secs(1);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        class: TurnErrorClass::PausedRateLimit,
        at: now,
        label: Some("usage limit reached".to_owned()),
    });
    child.context = Some(context);
    parent.pane = Some(pane.clone());
    let snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![parent.clone(), child],
        now,
    )
    .with_live_panes(vec![pane], None);
    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &parent,
        Some(&snapshot.worktree_groups[0].rows[0]),
        None,
        &[&parent],
        None,
        now,
        ReportOverrides::default(),
    );
    let json = serde_json::to_value(entry).unwrap();
    assert_eq!(json["status"], "paused");
    assert_eq!(json["phase"], "idle");
    assert!(json["turn_error"].is_null());
    assert_eq!(json["sub_agents"][0]["status"], "paused");
}

#[test]
fn projected_row_status_wins_and_rowless_error_falls_back_to_failed() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut state = agent("status");
    state.status = AgentStatus::Running;
    state.phase = TurnPhase::Acting;
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        class: TurnErrorClass::Failed,
        at: now,
        label: Some("boom".to_owned()),
    });
    state.context = Some(context);
    let row = SidebarRow {
        id: "status".to_owned(),
        name: "codex".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: now,
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Paused,
            phase: TurnPhase::Idle,
            ..AgentCard::default()
        })),
    };
    let peers = [&state];

    assert_eq!(
        build_entry(
            &rimz::agents::ParkDemotion::default(),
            &state,
            Some(&row),
            None,
            &peers,
            None,
            now,
            ReportOverrides::default(),
        )
        .status,
        AgentStatus::Paused
    );
    assert_eq!(
        build_entry(
            &rimz::agents::ParkDemotion::default(),
            &state,
            None,
            None,
            &peers,
            None,
            now,
            ReportOverrides::default(),
        )
        .status,
        AgentStatus::Failed
    );
}

#[test]
fn rowless_spent_child_reports_failed_with_provider_error() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut child = agent("child");
    child.status = AgentStatus::Running;
    child.last_activity = now - jiff::SignedDuration::from_secs(1);
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        class: TurnErrorClass::PausedRateLimit,
        at: now,
        label: Some("usage limit reached".to_owned()),
    });
    child.context = Some(context);
    let demotion = rimz::agents::ParkDemotion::new(
        &Default::default(),
        [(child.kind.clone(), child.agent_id.clone())].into(),
        now,
    );
    let entry = build_entry(
        &demotion,
        &child,
        None,
        None,
        &[&child],
        None,
        now,
        ReportOverrides::default(),
    );
    assert_eq!(entry.status, AgentStatus::Failed);
    assert_eq!(entry.phase, TurnPhase::Idle);
    let json = serde_json::to_value(entry).unwrap();
    assert_eq!(json["status"], "failed");
    assert_eq!(json["turn_error"]["class"], "paused_rate_limit");
    assert_eq!(json["turn_error"]["label"], "usage limit reached");
}

#[test]
fn report_cost_includes_delegated_spend() {
    let now = Timestamp::from_second(1_000).unwrap();
    let state = agent("spend");
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.cost = Some(rimz::agents::AgentCost {
        total_cost_usd: Some(0.40),
        ..rimz::agents::AgentCost::default()
    });
    let row = SidebarRow {
        id: "spend".to_owned(),
        name: "codex".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: now,
        card: RowCard::Agent(Box::new(AgentCard {
            context: Some(context),
            delegated_cost_usd: Some(0.60),
            ..AgentCard::default()
        })),
    };
    let peers = [&state];

    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        Some(&row),
        None,
        &peers,
        None,
        now,
        ReportOverrides::default(),
    );

    assert_eq!(entry.stats.cost_usd, Some(1.0));
}

#[test]
fn full_entry_has_a_stable_projection() {
    let now = Timestamp::from_second(2_000).unwrap();
    let mut state = agent("full");
    state.login = Some("work".parse().unwrap());
    state.launch_warnings = vec!["tool rules unsupported".into(), "skill shadowed".into()];
    state.name_explicit = true;
    state.profile = Some("builder".to_owned());
    state.role = Some("coder".to_owned());
    state.team = Some("forge".to_owned());
    state.mode = Some(PermissionMode::Yolo);
    state.channel = Some("auth".to_owned());
    state.worktree_path = Some("/repo/auth".to_owned());
    state.worktree_branch = Some("feature/auth".to_owned());
    state.model = Some("gpt-5".to_owned());
    state.effort = Some("high".to_owned());
    state.description = Some("ship\nrefresh".to_owned());
    state.usage.total_tokens = Some(54_210);
    state.usage.fresh_input_tokens = Some(4_180);
    state.usage.cache_read_input_tokens = Some(47_600);
    state.usage.cache_write_input_tokens = Some(1_920);
    state.usage.output_tokens = Some(510);
    state.usage.context_pct = Some(31);
    state.compaction_count = 2;
    state.tool_calls.insert("Bash".to_owned(), 12);
    state.registered_at = Some(Timestamp::from_second(1_000).unwrap());
    state.turn_started_at = Some(Timestamp::from_second(1_900).unwrap());
    state.budget = Some("$5.00".to_owned());
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.model_id = Some("gpt-5.5".to_owned());
    context.model_display_name = Some("GPT 5.5".to_owned());
    context.effort = Some("high".to_owned());
    context.cost = Some(rimz::agents::AgentCost {
        total_cost_usd: Some(0.87),
        ..rimz::agents::AgentCost::default()
    });
    state.context = Some(context.clone());
    let row = SidebarRow {
        id: "full".to_owned(),
        name: "codex".to_owned(),
        pane: None,
        worktree_path: state.worktree_path.clone(),
        worktree_branch: state.worktree_branch.clone(),
        channel: state.channel.clone(),
        unread: true,
        inactive: false,
        archived: false,
        attention_score: 42,
        last_activity: now,
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Running,
            phase: TurnPhase::Acting,
            context: Some(context),
            context_severity: Some(ContextSeverity::Yellow),
            sub_agent_count: 1,
            sub_agents: vec![SidebarSubAgent {
                pane: None,
                turn_error_label: None,
                id: "child".to_owned(),
                prior_turn: false,
                name: "explorer".to_owned(),
                petname: Some("swift-otter".to_owned()),
                provider_native: false,
                stalled: false,
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                task: None,
                profile: Some("explorer".to_owned()),
                model: Some("sonnet".to_owned()),
                effort: Some("high".to_owned()),
                description: None,
                tokens: Some(SubAgentTokens::Window(1_200)),
                context_window: None,
                cost_usd: None,
                elapsed_secs: Some(12),
                started_at: None,
                last_activity: now,
                registered_at: None,
            }],
            ..AgentCard::default()
        })),
    };
    let mut row = row;
    let card = row.as_agent_mut().unwrap();
    let mut prior = card.sub_agents[0].clone();
    prior.id = "prior-child".to_owned();
    prior.prior_turn = true;
    prior.status = AgentStatus::Success;
    card.sub_agents.push(prior);
    let peers = [&state];
    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        Some(&row),
        Some(PrInfo {
            number: Some(91),
            state: WorktreePrState::Open,
            ci: Some(WorktreeCi::Passing),
        }),
        &peers,
        Some(&state.agent_id),
        now,
        ReportOverrides {
            active_secs: Some(754),
            ..ReportOverrides::default()
        },
    );

    assert_eq!(
        serde_json::to_value(&entry).unwrap()["sub_agents"][0]["tokens"],
        serde_json::json!({"window": 1_200})
    );
    assert_eq!(
        serde_json::to_value(&entry).unwrap()["launch_warnings"],
        serde_json::json!(state.launch_warnings)
    );
    assert_eq!(serde_json::to_value(&entry).unwrap()["login"], "codex@work");
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        insta::assert_json_snapshot!("full_agent_report", entry);
    });
}

#[test]
fn sparse_entry_keeps_unknown_and_zero_keys() {
    let state = agent("sparse");
    let peers = [&state];
    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        None,
        None,
        &peers,
        None,
        Timestamp::UNIX_EPOCH,
        ReportOverrides::default(),
    );

    assert_eq!(
        serde_json::to_value(&entry).unwrap()["launch_warnings"],
        serde_json::json!([])
    );
    assert!(serde_json::to_value(&entry).unwrap().get("login").is_none());
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        insta::assert_json_snapshot!("sparse_agent_report", entry);
    });
}

#[test]
fn lifetime_effort_overrides_live_stats_as_one_unit() {
    let now = Timestamp::from_second(2_000).unwrap();
    let mut state = agent("effort");
    state.usage.total_tokens = Some(999);
    state.usage.fresh_input_tokens = Some(999);
    state.budget = Some("$1.00".to_owned());
    let peers = [&state];
    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        None,
        None,
        &peers,
        None,
        now,
        ReportOverrides {
            effort: Some(rimz::agents::spending::SlotEffort {
                tokens: rimz::agents::spending::EffortTokens {
                    input: 10,
                    output: 20,
                    cache_write: 30,
                    cache_read: 40,
                },
                cost_usd: Some(0.5),
            }),
            active_secs: Some(60),
            budget_cost_usd: Some(0.25),
            ..ReportOverrides::default()
        },
    );

    assert_eq!(entry.stats.total_tokens, Some(100));
    assert_eq!(entry.stats.fresh_input_tokens, Some(10));
    assert_eq!(entry.stats.output_tokens, Some(20));
    assert_eq!(entry.stats.cache_write_tokens, Some(30));
    assert_eq!(entry.stats.cache_read_tokens, Some(40));
    assert_eq!(entry.stats.cost_usd, Some(0.5));
    assert_eq!(entry.stats.active_secs, Some(60));
    assert_eq!(entry.budget.spent_usd, Some(0.25));
}

#[test]
fn budget_projection_uses_effective_ledger_cap_and_live_spend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = rimz::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = rimz::RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let now = Timestamp::from_second(2_000).unwrap();
    let mut state = agent("budget");
    state.budget = Some("$9.00".to_owned());
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.cost = Some(rimz::agents::AgentCost {
        total_cost_usd: Some(7.25),
        ..rimz::agents::AgentCost::default()
    });
    state.context = Some(context);
    let mut ledger = rimz::harness::budget::BudgetLedger::new("5/day".parse().expect("spec"));
    ledger.raised_cap_usd = Some(6.0);
    ledger.day_baseline = Some(rimz::harness::budget::DayBaseline {
        date: "2026-06-01".parse().expect("date"),
        cost_usd: 2.0,
    });
    rimz::harness::budget::write_ledger(&runtime, &state.kind, &state.agent_id, &ledger)
        .expect("write ledger");
    let peers = [&state];

    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        None,
        None,
        &peers,
        None,
        now,
        ReportOverrides {
            runtime: Some(&runtime),
            effort: Some(rimz::agents::spending::SlotEffort {
                cost_usd: Some(100.0),
                ..rimz::agents::spending::SlotEffort::default()
            }),
            ..ReportOverrides::default()
        },
    );

    assert_eq!(entry.budget.cap.as_deref(), Some("$6.00/day"));
    assert_eq!(entry.budget.spent_usd, Some(5.25));

    let snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(dir.path()),
        vec![state.clone()],
        now,
    );
    let agents = rimz::address::addressable_agents(&snapshot);
    let list = build_list_report(
        &rimz::agents::ParkDemotion::default(),
        &snapshot,
        &agents,
        now,
        Some(&runtime),
        &[],
        &Default::default(),
    );
    assert_eq!(list.agents[0].budget.cap.as_deref(), Some("$6.00/day"));
    assert_eq!(list.agents[0].budget.spent_usd, Some(5.25));

    ledger.disabled = true;
    rimz::harness::budget::write_ledger(&runtime, &state.kind, &state.agent_id, &ledger)
        .expect("disable ledger");
    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        None,
        None,
        &peers,
        None,
        now,
        ReportOverrides {
            runtime: Some(&runtime),
            ..ReportOverrides::default()
        },
    );
    assert_eq!(entry.budget.cap, None);
    assert_eq!(entry.budget.spent_usd, None);
}
