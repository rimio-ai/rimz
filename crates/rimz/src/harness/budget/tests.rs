use super::*;
use crate::agents::{AgentContext, AgentCost};

fn agent(cost: f64, status: AgentStatus, turn_started_at: Option<Timestamp>) -> AgentState {
    let now = Timestamp::from_second(100).expect("timestamp");
    let mut agent = AgentState::stub("claude", "sess", status);
    agent.turn_started_at = turn_started_at;
    agent.context = Some(AgentContext {
        source: "test".to_owned(),
        cost: Some(AgentCost {
            total_cost_usd: Some(cost),
            ..AgentCost::default()
        }),
        observed_at: now,
        ..crate::agents::AgentContext::new("test", now)
    });
    agent
}

#[test]
fn budget_spec_accepts_canonical_forms_and_rejects_bad_values() {
    for (raw, cap, window, display) in [
        ("5", 5.0, BudgetWindow::Session, "$5.00"),
        ("$4.50", 4.5, BudgetWindow::Session, "$4.50"),
        ("20/day", 20.0, BudgetWindow::Day, "$20.00/day"),
    ] {
        let spec: BudgetSpec = raw.parse().expect(raw);
        assert_eq!(spec.cap_usd, cap);
        assert_eq!(spec.window, window);
        assert_eq!(spec.to_string(), display);
    }
    for raw in ["", "$", "+5", "-1", "NaN", "5/week", "1/day/nope"] {
        assert!(raw.parse::<BudgetSpec>().is_err(), "{raw}");
    }
}

#[test]
fn budget_cache_path_preserves_existing_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .expect("runtime");
    assert_eq!(
        budget_ledger_path(
            &runtime,
            &AgentKind::new_unchecked("claude"),
            &"sess".into()
        )
        .file_name()
        .and_then(|name| name.to_str()),
        Some("budget.4a8d94f232e55a6a0879ba0858b59241.json")
    );
}

#[test]
fn current_usage_cost_cannot_trigger_budget_enforcement() {
    let now = Timestamp::from_second(200).expect("timestamp");
    let mut current_usage = agent(99.0, AgentStatus::Idle, Some(now));
    current_usage.kind = crate::ids::AgentKind::new_unchecked("antigravity");
    current_usage
        .context
        .as_mut()
        .and_then(|context| context.cost.as_mut())
        .unwrap()
        .coverage = crate::agents::CostCoverage::CurrentUsage;
    let mut ledger = BudgetLedger::new("1".parse().expect("spec"));

    assert_eq!(total_cost_usd(&current_usage), None);
    assert!(matches!(
        evaluate(&current_usage, &mut ledger, now, &TimeZone::UTC, None),
        BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
    ));
    assert!(ledger.parked.is_none());
}

#[test]
fn session_cost_triggers_budget_enforcement() {
    let now = Timestamp::from_second(200).expect("timestamp");
    let mut priced = agent(99.0, AgentStatus::Idle, Some(now));
    priced.kind = crate::ids::AgentKind::new_unchecked("droid");
    priced
        .context
        .as_mut()
        .and_then(|context| context.cost.as_mut())
        .unwrap()
        .coverage = crate::agents::CostCoverage::Session;
    let mut ledger = BudgetLedger::new("1".parse().expect("spec"));

    assert_eq!(total_cost_usd(&priced), Some(99.0));
    assert!(matches!(
        evaluate(&priced, &mut ledger, now, &TimeZone::UTC, None),
        BudgetVerdict::Park { spend_usd, .. } if spend_usd == 99.0
    ));
}

#[test]
fn spend_summary_uses_ledger_cap_window_and_park_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let mut state = agent(7.25, AgentStatus::Idle, None);
    state.budget = Some("9".to_owned());
    let mut ledger = BudgetLedger::new("5/day".parse().expect("spec"));
    ledger.raised_cap_usd = Some(6.0);
    ledger.day_baseline = Some(DayBaseline {
        date: "2026-06-01".parse().expect("date"),
        cost_usd: 2.0,
    });
    write_ledger(&runtime, &state.kind, &state.agent_id, &ledger).expect("write ledger");

    let spend = agent_budget_spend(&runtime, &state, Some(100.0));
    let cap = spend.cap.expect("ledger cap");
    assert_eq!(
        cap.window
            .fmt_spend(spend.spent_usd.unwrap_or(0.0), cap.cap_usd),
        "$5.25 of $6.00/day",
        "ledger spec and observed agent cost take precedence"
    );

    state.budget_park = Some(BudgetPark {
        cap_usd: 4.0,
        spend_usd: 4.5,
        window: BudgetWindow::Day,
        at: Timestamp::from_second(100).expect("timestamp"),
        scope: BudgetScope::Fleet,
        account_kind: None,
        resets_at: None,
    });
    let spend = agent_budget_spend(&runtime, &state, None);
    let cap = spend.cap.expect("park cap");
    assert_eq!(
        cap.window
            .fmt_spend(spend.spent_usd.unwrap_or(0.0), cap.cap_usd),
        "$4.50 of $4.00/day"
    );
}

#[test]
fn absolute_budget_parks_and_one_human_delivery_waives_one_turn() {
    let zone = TimeZone::UTC;
    let now = Timestamp::from_second(200).expect("timestamp");
    let mut ledger = BudgetLedger::new("5".parse().expect("spec"));
    let idle = agent(6.0, AgentStatus::Idle, Some(now));
    assert!(matches!(
        evaluate(&idle, &mut ledger, now, &zone, None),
        BudgetVerdict::Park { .. }
    ));
    let delivered = Timestamp::from_second(201).expect("timestamp");
    let running = agent(6.0, AgentStatus::Running, Some(delivered));
    assert!(matches!(
        evaluate(
            &running,
            &mut ledger,
            Timestamp::from_second(202).expect("timestamp"),
            &zone,
            Some(delivered)
        ),
        BudgetVerdict::Waived { .. }
    ));
    let idle = agent(6.0, AgentStatus::Idle, Some(delivered));
    assert!(matches!(
        evaluate(
            &idle,
            &mut ledger,
            Timestamp::from_second(203).expect("timestamp"),
            &zone,
            Some(delivered)
        ),
        BudgetVerdict::Park { .. }
    ));
    assert!(
        ledger
            .parked
            .as_ref()
            .is_some_and(|park| park.at.as_second() == 203)
    );
}

#[test]
fn day_budget_rebases_on_first_sight_and_when_local_date_advances() {
    let zone = TimeZone::UTC;
    let first = "2026-06-01T23:59:00Z".parse().expect("timestamp");
    let next = "2026-06-02T00:01:00Z".parse().expect("timestamp");
    let mut ledger = BudgetLedger::new("5/day".parse().expect("spec"));
    let resumed = agent(6.0, AgentStatus::Running, Some(first));
    assert!(matches!(
        evaluate(&resumed, &mut ledger, first, &zone, None),
        BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
    ));
    let over = agent(12.0, AgentStatus::Running, Some(first));
    assert!(matches!(
        evaluate(&over, &mut ledger, first, &zone, None),
        BudgetVerdict::Park { .. }
    ));
    let reset = agent(12.5, AgentStatus::Idle, Some(next));
    assert!(matches!(
        evaluate(&reset, &mut ledger, next, &zone, None),
        BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
    ));
    assert!(ledger.parked.is_none());
}

#[test]
fn turn_budget_parks_at_cap_and_rebases_on_a_new_turn() {
    let first = Timestamp::from_second(200).expect("first turn");
    let second = Timestamp::from_second(300).expect("second turn");
    let now = Timestamp::from_second(201).expect("now");
    let mut entry = None;

    assert!(matches!(
        evaluate_turn_scope(
            &agent(10.0, AgentStatus::Running, Some(first)),
            &mut entry,
            3.0,
            first,
        ),
        BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
    ));
    assert!(matches!(
        evaluate_turn_scope(
            &agent(13.0, AgentStatus::Running, Some(first)),
            &mut entry,
            3.0,
            now,
        ),
        BudgetVerdict::Park { spend_usd, .. } if spend_usd == 3.0
    ));
    entry.as_mut().expect("turn entry").last_interrupt_at = Some(now);

    assert!(matches!(
        evaluate_turn_scope(
            &agent(13.5, AgentStatus::Running, Some(second)),
            &mut entry,
            3.0,
            second,
        ),
        BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
    ));
    let entry = entry.expect("rebased turn entry");
    assert_eq!(entry.baseline_cost_usd, 13.5);
    assert!(entry.parked.is_none());
    assert!(entry.last_interrupt_at.is_none());
}

#[test]
fn first_tick_of_a_new_turn_cannot_reinterrupt_prior_turn_spend() {
    let first = Timestamp::from_second(200).expect("first turn");
    let second = Timestamp::from_second(300).expect("second turn");
    let mut entry = Some(TurnScopeEntry {
        turn_started_at: first,
        baseline_cost_usd: 0.0,
        parked: Some(BudgetParkStamp {
            at_cost: 50.0,
            at: first,
        }),
        last_interrupt_at: Some(first),
    });

    assert!(matches!(
        evaluate_turn_scope(
            &agent(50.25, AgentStatus::Running, Some(second)),
            &mut entry,
            3.0,
            second,
        ),
        BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
    ));
    let entry = entry.expect("rebased turn entry");
    assert_eq!(entry.baseline_cost_usd, 50.25);
    assert!(entry.parked.is_none());
    assert!(entry.last_interrupt_at.is_none());
}

#[test]
fn turn_budget_clears_midnight_auto_continue_parks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let config: MachineConfig = toml::from_str("[harness]\nturn_budget = \"5\"\n").expect("config");
    let first = Timestamp::from_second(200).expect("first turn");
    let mut state = agent(6.0, AgentStatus::Running, Some(first));
    state.last_activity = first;
    write_scope_state(
        &runtime,
        &BudgetScopeState {
            turn: BTreeMap::from([(
                scope_agent_key(&state),
                TurnScopeEntry {
                    turn_started_at: first,
                    baseline_cost_usd: 0.0,
                    parked: None,
                    last_interrupt_at: None,
                },
            )]),
            ..Default::default()
        },
    )
    .expect("scope state");
    let arm = |state: &AgentState| {
        crate::harness::auto_continue::arm_budget_park(
            &runtime,
            &state.kind,
            &state.agent_id,
            Timestamp::from_second(86_400).expect("deadline"),
            state.last_activity,
        );
    };

    arm(&state);
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id.clone(), vec![state.clone()], first);
    snapshot.agent_panes = vec![crate::store::snapshot::PaneAgent {
        kind: state.kind.clone(),
        kind_ordinal: state.kind_ordinal,
        name: state.name.clone(),
        name_explicit: state.name_explicit,
        profile: state.profile.clone(),
        role: state.role.clone(),
        channel: state.channel.clone(),
        agent_id: Some(state.agent_id.clone()),
        pane_id: PaneId::from_parts(crate::MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    }];
    enforce(&snapshot, &runtime, None, &config);
    assert!(
        read_scope_state(&runtime)
            .turn
            .get(&scope_agent_key(&state))
            .is_some_and(|entry| entry.last_interrupt_at == Some(first))
    );
    assert!(
        !crate::harness::auto_continue::budget_park_armed(&runtime, &state.kind, &state.agent_id,),
        "a turn-only park has no midnight resume"
    );

    let second = Timestamp::from_second(300).expect("second turn");
    state.turn_started_at = Some(second);
    arm(&state);
    let snapshot = SidebarSnapshot::build_with_agents(workspace_id, vec![state.clone()], second);
    enforce(&snapshot, &runtime, None, &config);
    assert!(
        !crate::harness::auto_continue::budget_park_armed(&runtime, &state.kind, &state.agent_id,),
        "all-under includes a rebased turn verdict"
    );
}

#[test]
fn turn_park_projection_ignores_a_stale_turn_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let config: MachineConfig = toml::from_str("[harness]\nturn_budget = \"5\"\n").expect("config");
    let first = Timestamp::from_second(200).expect("first turn");
    let second = Timestamp::from_second(300).expect("second turn");
    let state = agent(6.0, AgentStatus::Running, Some(second));
    write_scope_state(
        &runtime,
        &BudgetScopeState {
            turn: BTreeMap::from([(
                scope_agent_key(&state),
                TurnScopeEntry {
                    turn_started_at: first,
                    baseline_cost_usd: 0.0,
                    parked: Some(BudgetParkStamp {
                        at_cost: 6.0,
                        at: first,
                    }),
                    last_interrupt_at: Some(first),
                },
            )]),
            ..Default::default()
        },
    )
    .expect("scope state");

    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id, vec![state], second);
    project_parks(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
    );

    assert!(snapshot.agents[0].budget_park.is_none());
}

#[test]
fn disabling_turn_budget_clears_turn_scope_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let turn = Timestamp::from_second(200).expect("turn");
    let state = agent(6.0, AgentStatus::Idle, Some(turn));
    write_scope_state(
        &runtime,
        &BudgetScopeState {
            turn: BTreeMap::from([(
                scope_agent_key(&state),
                TurnScopeEntry {
                    turn_started_at: turn,
                    baseline_cost_usd: 0.0,
                    parked: None,
                    last_interrupt_at: None,
                },
            )]),
            ..Default::default()
        },
    )
    .expect("scope state");
    let snapshot = SidebarSnapshot::build_with_agents(workspace_id, Vec::new(), turn);

    enforce(&snapshot, &runtime, None, &MachineConfig::default());

    assert!(read_scope_state(&runtime).turn.is_empty());
}

#[test]
fn active_absolute_waiver_hides_the_paused_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let delivered = Timestamp::from_second(201).expect("timestamp");
    let mut running = agent(6.0, AgentStatus::Running, Some(delivered));
    let mut ledger = BudgetLedger::new("5".parse().expect("spec"));
    ledger.parked = Some(BudgetParkStamp {
        at_cost: 6.0,
        at: Timestamp::from_second(200).expect("timestamp"),
    });
    ledger.last_interrupt_at = Some(Timestamp::from_second(200).expect("timestamp"));
    ledger.waived_delivery_at = Some(delivered);
    write_ledger(&runtime, &running.kind, &running.agent_id, &ledger).expect("write ledger");

    let mut snapshot = SidebarSnapshot::build_with_agents(
        workspace_id,
        vec![running.clone()],
        Timestamp::from_second(202).expect("timestamp"),
    );
    project_parks(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &MachineConfig::default(),
    );
    assert!(snapshot.agents[0].budget_park.is_none());

    running.status = AgentStatus::Idle;
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![running],
        Timestamp::from_second(203).expect("timestamp"),
    );
    project_parks(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &MachineConfig::default(),
    );
    assert!(snapshot.agents[0].budget_park.is_some());
}

#[test]
fn fleet_park_projects_only_live_or_interrupted_agents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let config: MachineConfig =
        toml::from_str("timezone = \"UTC\"\n[harness]\nbudget = \"5/day\"\n").expect("config");
    let now = Timestamp::from_second(200).expect("timestamp");
    DailyBudgetScope::Fleet
        .merge_park(
            &runtime,
            Some(BudgetParkStamp {
                at_cost: 6.0,
                at: now,
            }),
        )
        .expect("fleet ledger");
    assert!(
        !state_paths(&runtime).fleet_budget_record.exists(),
        "producer park must not create standing choices"
    );

    let state = |id: &str, status| {
        let mut state = agent(0.0, status, Some(now));
        state.agent_id = AgentSessionId::from(id);
        state
    };
    let idle = state("idle", AgentStatus::Idle);
    let success = state("success", AgentStatus::Success);
    let waiting = state("waiting", AgentStatus::Waiting);
    let running = state("running", AgentStatus::Running);
    let interrupted_idle = state("interrupted-idle", AgentStatus::Idle);
    let interrupted_waiting = state("interrupted-waiting", AgentStatus::Waiting);
    let scope_state = BudgetScopeState {
        last_interrupt_at: BTreeMap::from([
            (scope_agent_key(&interrupted_idle), now),
            (scope_agent_key(&interrupted_waiting), now),
        ]),
        ..Default::default()
    };
    write_scope_state(&runtime, &scope_state).expect("scope state");

    let mut snapshot = SidebarSnapshot::build_with_agents(
        workspace_id,
        vec![
            idle,
            success,
            waiting,
            running,
            interrupted_idle,
            interrupted_waiting,
        ],
        now,
    );
    project_parks(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
    );
    let projected = snapshot
        .agents
        .iter()
        .map(|agent| (agent.agent_id.as_str(), agent.budget_park.is_some()))
        .collect::<BTreeMap<_, _>>();
    assert!(!projected["idle"]);
    assert!(!projected["success"]);
    assert!(!projected["waiting"]);
    assert!(projected["running"]);
    assert!(projected["interrupted-idle"]);
    assert!(!projected["interrupted-waiting"]);
}

#[test]
fn fleet_enforcement_arms_resume_after_interrupting_a_running_agent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let config: MachineConfig =
        toml::from_str("timezone = \"UTC\"\n[harness]\nbudget = \"5/day\"\n").expect("config");
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().expect("timestamp");
    let cutoff = local_day_start(now, &TimeZone::UTC)
        .expect("day cutoff")
        .as_second() as u64;
    let idle = agent(0.0, AgentStatus::Idle, Some(now));
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id.clone(), vec![idle.clone()], now);
    snapshot.fleet_day_spend_usd = Some(6.0);
    snapshot.fleet_day_spend_epoch_secs = Some(cutoff);

    enforce(&snapshot, &runtime, None, &config);
    assert!(!crate::harness::auto_continue::budget_park_armed(
        &runtime,
        &idle.kind,
        &idle.agent_id,
    ));

    let mut running = idle;
    running.status = AgentStatus::Running;
    running.parent_agent_id = Some("parent".into());
    running.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
    running.launch_depth = Some(1);
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id, vec![running.clone()], now);
    snapshot.fleet_day_spend_usd = Some(6.0);
    snapshot.fleet_day_spend_epoch_secs = Some(cutoff);
    snapshot.agent_panes = vec![crate::store::snapshot::PaneAgent {
        kind: running.kind.clone(),
        kind_ordinal: running.kind_ordinal,
        name: running.name.clone(),
        name_explicit: running.name_explicit,
        profile: running.profile.clone(),
        role: running.role.clone(),
        channel: running.channel.clone(),
        agent_id: Some(running.agent_id.clone()),
        pane_id: PaneId::from_parts(crate::MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    }];

    enforce(&snapshot, &runtime, None, &config);
    assert!(scope_interrupted(&read_scope_state(&runtime), &running));
    assert!(crate::harness::auto_continue::budget_park_armed(
        &runtime,
        &running.kind,
        &running.agent_id,
    ));
}

#[test]
fn interrupt_retry_is_throttled_for_two_minutes() {
    let at = Timestamp::from_second(1_000).expect("timestamp");
    assert!(!interrupt_due(
        Some(at),
        Timestamp::from_second(1_119).expect("timestamp")
    ));
    assert!(interrupt_due(
        Some(at),
        Timestamp::from_second(1_120).expect("timestamp")
    ));
}

#[test]
fn only_interactive_human_delivery_waives_a_budget() {
    let state = agent(0.0, AgentStatus::Idle, None);
    let mut message = MessageRecord::new(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/budget")),
        &state,
        "continue".to_owned(),
        DeliveryGate::Done,
    );
    message.status = MessageStatus::Delivered;
    assert!(is_budget_waiving_delivery(&message));

    message.automated = true;
    assert!(!is_budget_waiving_delivery(&message));
    message.automated = false;
    message.gate = DeliveryGate::Resume;
    assert!(!is_budget_waiving_delivery(&message));
    message.gate = DeliveryGate::Done;
    message.sender = MessageSender::Agent {
        agent_id: None,
        kind: state.kind,
        name: None,
        profile: None,
        role: None,
        channel: None,
    };
    assert!(!is_budget_waiving_delivery(&message));
    message.sender = MessageSender::System;
    assert!(!is_budget_waiving_delivery(&message));
}

#[test]
fn daily_scopes_park_and_reopen_when_spend_resets() {
    let now = Timestamp::from_second(1_000).expect("timestamp");
    let mut parked = None;
    assert!(matches!(
        evaluate_daily_scope(&mut parked, Some(5.0), 4.99, now),
        BudgetVerdict::Under { .. }
    ));
    assert!(matches!(
        evaluate_daily_scope(&mut parked, Some(5.0), 5.0, now),
        BudgetVerdict::Park { .. }
    ));
    assert_eq!(parked.as_ref().map(|park| park.at_cost), Some(5.0));
    assert!(matches!(
        evaluate_daily_scope(
            &mut parked,
            Some(5.0),
            0.25,
            Timestamp::from_second(2_000).expect("timestamp")
        ),
        BudgetVerdict::Under { .. }
    ));
    assert!(parked.is_none());
}

#[test]
fn scope_waiver_is_consumed_after_exactly_one_turn() {
    let parked = Timestamp::from_second(200).expect("parked");
    let delivered = Timestamp::from_second(201).expect("delivered");
    let mut state = BudgetScopeState::default();
    let idle = agent(0.0, AgentStatus::Idle, Some(parked));
    assert_eq!(
        evaluate_scope_waiver(&idle, Some(parked), Some(delivered), &mut state, delivered),
        ScopeAgentVerdict::Park
    );
    let running = agent(0.0, AgentStatus::Running, Some(delivered));
    assert_eq!(
        evaluate_scope_waiver(
            &running,
            Some(parked),
            Some(delivered),
            &mut state,
            Timestamp::from_second(202).expect("timestamp")
        ),
        ScopeAgentVerdict::Waived
    );
    let finished = agent(0.0, AgentStatus::Idle, Some(delivered));
    assert_eq!(
        evaluate_scope_waiver(
            &finished,
            Some(parked),
            Some(delivered),
            &mut state,
            Timestamp::from_second(203).expect("timestamp")
        ),
        ScopeAgentVerdict::Park
    );
    let next = agent(
        0.0,
        AgentStatus::Running,
        Some(Timestamp::from_second(204).expect("timestamp")),
    );
    assert_eq!(
        evaluate_scope_waiver(
            &next,
            Some(parked),
            Some(delivered),
            &mut state,
            Timestamp::from_second(204).expect("timestamp")
        ),
        ScopeAgentVerdict::Park
    );
    let later_park = Timestamp::from_second(205).expect("later park");
    assert_eq!(
        evaluate_scope_waiver(
            &next,
            Some(later_park),
            Some(Timestamp::from_second(204).expect("old delivery")),
            &mut state,
            later_park,
        ),
        ScopeAgentVerdict::Park
    );
    assert_eq!(state.parked_at.get("claude:sess"), Some(&later_park));
}

#[test]
fn scope_ledgers_round_trip_and_labels_name_the_binding_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        &dir.path().join("runtime"),
    )
    .expect("runtime");
    let state =
        crate::StatePaths::under(runtime.workspace_id.clone(), &dir.path().join("state")).unwrap();
    let store = Store::open(state, runtime).unwrap();
    let runtime = store.runtime_paths().clone();
    runtime.ensure_dirs().expect("dirs");
    let fleet = DailyBudgetLedger {
        override_spec: Some("20/day".parse().expect("spec")),
        raised_cap_usd: Some(25.0),
        disabled: false,
        parked: Some(BudgetParkStamp {
            at_cost: 25.5,
            at: Timestamp::from_second(100).expect("timestamp"),
        }),
    };
    let legacy_fleet: DailyBudgetLedger = serde_json::from_str(
        r#"{"override_spec":{"cap_usd":20.0,"window":"day"},"raised_cap_usd":25.0,"parked":{"at_cost":25.5,"at":"1970-01-01T00:01:40Z"}}"#,
    )
    .expect("legacy fleet ledger");
    assert_eq!(legacy_fleet, fleet);
    let fleet_scope = DailyBudgetScope::Fleet;
    assert_eq!(
        fleet_scope.ledger_path(&runtime, store.paths()),
        store.paths().fleet_budget_record
    );
    fleet_scope
        .write_ledger(&runtime, store.paths(), &fleet)
        .expect("fleet write");
    let record = std::fs::read(fleet_scope.ledger_path(&runtime, store.paths())).unwrap();
    let modified = std::fs::metadata(fleet_scope.ledger_path(&runtime, store.paths()))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        fleet_scope
            .read_ledger(&runtime, Some(store.paths()))
            .parked
            .is_none(),
        "CLI mutation clears the producer park"
    );
    fleet_scope
        .merge_park(&runtime, fleet.parked.clone())
        .unwrap();
    assert_eq!(
        fleet_scope.read_ledger(&runtime, Some(store.paths())),
        fleet
    );
    fleet_scope
        .merge_park(&runtime, None)
        .expect("merge fleet park");
    assert_eq!(
        fleet_scope.read_ledger(&runtime, Some(store.paths())),
        DailyBudgetLedger {
            parked: None,
            ..fleet.clone()
        },
        "producer park writes preserve CLI cap overrides"
    );
    assert_eq!(
        std::fs::read(fleet_scope.ledger_path(&runtime, store.paths())).unwrap(),
        record,
        "producer never rewrites the choices record"
    );
    assert_eq!(
        std::fs::metadata(fleet_scope.ledger_path(&runtime, store.paths()))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    store.reset_records(false).unwrap();
    assert_eq!(
        fleet_scope.read_ledger(store.runtime_paths(), Some(store.paths())),
        DailyBudgetLedger {
            parked: None,
            ..fleet.clone()
        }
    );

    let kind = AgentKind::new_unchecked("claude");
    let account = DailyBudgetLedger {
        override_spec: None,
        raised_cap_usd: Some(100.0),
        disabled: false,
        parked: fleet.parked.clone(),
    };
    let legacy_account: DailyBudgetLedger = serde_json::from_str(
        r#"{"raised_cap_usd":100.0,"parked":{"at_cost":25.5,"at":"1970-01-01T00:01:40Z"}}"#,
    )
    .expect("legacy account ledger");
    assert_eq!(legacy_account, account);
    assert!(
        !serde_json::to_value(&account)
            .expect("account json")
            .as_object()
            .expect("account object")
            .contains_key("override_spec"),
        "account ledgers do not invent a fleet override"
    );
    let account_scope = DailyBudgetScope::Account(LoginKey::default_for(kind.clone()));
    assert_eq!(
        account_scope.ledger_path(&runtime, store.paths()),
        runtime
            .persistent_shared_root
            .join("budget.account.claude@default.json")
    );
    account_scope
        .write_ledger(&runtime, store.paths(), &account)
        .expect("account write");
    assert!(
        !std::fs::read_to_string(account_scope.ledger_path(&runtime, store.paths()))
            .expect("account ledger json")
            .contains("override_spec")
    );
    assert_eq!(
        account_scope.read_ledger(&runtime, Some(store.paths())),
        account
    );
    account_scope
        .merge_park(&runtime, None)
        .expect("merge account park");
    assert_eq!(
        account_scope.read_ledger(&runtime, Some(store.paths())),
        DailyBudgetLedger {
            parked: None,
            ..account.clone()
        },
        "producer park writes preserve CLI cap raises"
    );

    let fleet_label = BudgetPark {
        cap_usd: 25.0,
        spend_usd: 25.5,
        window: BudgetWindow::Day,
        at: Timestamp::from_second(100).expect("timestamp"),
        scope: BudgetScope::Fleet,
        account_kind: None,
        resets_at: None,
    };
    assert_eq!(fleet_label.label(), "fleet budget: $25.50 of $25.00/day");
    assert_eq!(
        BudgetPark {
            cap_usd: 3.0,
            spend_usd: 3.25,
            window: BudgetWindow::Turn,
            scope: BudgetScope::Turn,
            ..fleet_label.clone()
        }
        .label(),
        "turn budget: $3.25 of $3.00/turn"
    );
    assert_eq!(
        BudgetPark {
            scope: BudgetScope::Account,
            account_kind: Some(kind),
            ..fleet_label
        }
        .label(),
        "claude account budget: $25.50 of $25.00/day"
    );
}

#[test]
fn fleet_choices_use_explicit_state_without_opening_a_store() {
    let dir = tempfile::tempdir().unwrap();
    let id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(id.clone(), &dir.path().join("runtime")).unwrap();
    let state = crate::StatePaths::under_named(
        id.clone(),
        runtime.dir_name.clone(),
        &dir.path().join("state"),
    );
    let scope = DailyBudgetScope::Fleet;
    let ledger = DailyBudgetLedger {
        override_spec: Some("20/day".parse().unwrap()),
        ..Default::default()
    };
    scope.write_ledger(&runtime, &state, &ledger).unwrap();
    assert_eq!(scope.read_ledger(&runtime, Some(&state)), ledger);
    let mut snapshot = SidebarSnapshot::build_with_agents(id, Vec::new(), Timestamp::now());
    let config = toml::from_str("[harness]\nbudget = \"5/day\"\n").unwrap();
    project_budget_views(
        &mut snapshot,
        &runtime,
        Some(&state),
        &config,
        &Default::default(),
        &RoomLoginSet::new(None, None, BTreeMap::new()),
    );
    assert_eq!(snapshot.fleet_budget.unwrap().cap_usd, 20.0);
    assert!(state.fleet_budget_record.is_file());
}

#[test]
fn scope_ledgers_require_config_to_arm_runtime_caps() {
    let kind = AgentKind::new_unchecked("claude");
    let fleet = DailyBudgetLedger {
        override_spec: Some("20/day".parse().expect("spec")),
        raised_cap_usd: Some(25.0),
        ..Default::default()
    };
    let account = DailyBudgetLedger {
        raised_cap_usd: Some(100.0),
        ..Default::default()
    };
    let unarmed = MachineConfig::default();

    let fleet_scope = DailyBudgetScope::Fleet;
    let account_scope = DailyBudgetScope::Account(LoginKey::default_for(kind.clone()));
    assert_eq!(fleet_scope.effective_cap_usd(&fleet, &unarmed), None);
    assert_eq!(
        fleet_scope.cap_source(&fleet, &unarmed),
        BudgetCapSource::None
    );
    assert_eq!(account_scope.effective_cap_usd(&account, &unarmed), None);
    assert_eq!(
        account_scope.cap_source(&account, &unarmed),
        BudgetCapSource::None
    );

    let armed: MachineConfig =
        toml::from_str("[harness]\nbudget = \"10/day\"\n[accounts.budget]\nclaude = \"50/day\"\n")
            .expect("config");
    assert_eq!(fleet_scope.effective_cap_usd(&fleet, &armed), Some(25.0));
    assert_eq!(
        fleet_scope.cap_source(&fleet, &armed),
        BudgetCapSource::Raised
    );
    assert_eq!(
        account_scope.effective_cap_usd(&account, &armed),
        Some(100.0)
    );
    assert_eq!(
        account_scope.cap_source(&account, &armed),
        BudgetCapSource::Raised
    );
}

#[test]
fn unsupported_account_budget_is_ignored_by_projection_and_enforcement() {
    let kind = AgentKind::new_unchecked("antigravity");
    let config: MachineConfig =
        toml::from_str("[accounts.budget]\nantigravity = \"50/day\"\n").expect("config");
    let ledger = DailyBudgetLedger {
        raised_cap_usd: Some(100.0),
        parked: Some(BudgetParkStamp {
            at_cost: 150.0,
            at: Timestamp::from_second(100).expect("timestamp"),
        }),
        ..Default::default()
    };
    let scope = DailyBudgetScope::Account(LoginKey::default_for(kind));
    assert_eq!(scope.effective_cap_usd(&ledger, &config), None);
    assert_eq!(scope.cap_source(&ledger, &config), BudgetCapSource::None);
}

#[test]
fn park_projection_uses_agent_then_turn_then_fleet_then_account_precedence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("dirs");
    let config: MachineConfig = toml::from_str(
        "timezone = \"UTC\"\n[harness]\nbudget = \"10/day\"\nturn_budget = \"7\"\n[accounts.budget]\nclaude = \"20/day\"\n",
    )
    .expect("config");
    let now = Timestamp::from_second(200).expect("timestamp");
    let state = agent(8.0, AgentStatus::Idle, Some(now));
    let parked = BudgetParkStamp {
        at_cost: 30.0,
        at: now,
    };

    let mut agent_ledger = BudgetLedger::new("5".parse().expect("spec"));
    agent_ledger.parked = Some(parked.clone());
    agent_ledger.last_interrupt_at = Some(now);
    write_ledger(&runtime, &state.kind, &state.agent_id, &agent_ledger).expect("agent ledger");
    DailyBudgetScope::Fleet
        .merge_park(&runtime, Some(parked.clone()))
        .expect("fleet ledger");
    DailyBudgetScope::Account(LoginKey::default_for(state.kind.clone()))
        .write_ledger(
            &runtime,
            &state_paths(&runtime),
            &DailyBudgetLedger {
                parked: Some(parked),
                ..Default::default()
            },
        )
        .expect("account ledger");
    write_scope_state(
        &runtime,
        &BudgetScopeState {
            turn: BTreeMap::from([(
                scope_agent_key(&state),
                TurnScopeEntry {
                    turn_started_at: now,
                    baseline_cost_usd: 0.0,
                    parked: Some(BudgetParkStamp {
                        at_cost: 7.0,
                        at: now,
                    }),
                    last_interrupt_at: Some(now),
                },
            )]),
            last_interrupt_at: BTreeMap::from([(scope_agent_key(&state), now)]),
            ..Default::default()
        },
    )
    .expect("scope state");

    let projected_scope = |state: &AgentState| {
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id.clone(), vec![state.clone()], now);
        project_parks(
            &mut snapshot,
            &runtime,
            Some(&state_paths(&runtime)),
            &config,
        );
        snapshot.agents[0]
            .budget_park
            .as_ref()
            .map(|park| park.scope)
    };
    assert_eq!(projected_scope(&state), Some(BudgetScope::Agent));

    std::fs::remove_file(budget_ledger_path(&runtime, &state.kind, &state.agent_id))
        .expect("remove agent ledger");
    assert_eq!(projected_scope(&state), Some(BudgetScope::Turn));

    let mut scope_state = read_scope_state(&runtime);
    scope_state.turn.clear();
    write_scope_state(&runtime, &scope_state).expect("clear turn park");
    assert_eq!(projected_scope(&state), Some(BudgetScope::Fleet));

    let mut fleet = DailyBudgetScope::Fleet.read_ledger(&runtime, Some(&state_paths(&runtime)));
    fleet.disabled = true;
    DailyBudgetScope::Fleet
        .write_ledger(&runtime, &state_paths(&runtime), &fleet)
        .expect("disable fleet");
    assert_eq!(projected_scope(&state), Some(BudgetScope::Account));
}

#[test]
fn scope_gate_reads_room_and_account_local_day_caches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .expect("runtime");
    runtime.ensure_dirs().expect("dirs");
    let config: MachineConfig = toml::from_str(
            "timezone = \"UTC\"\n[harness]\nbudget = \"5/day\"\n[accounts.budget]\nclaude = \"10/day\"\n",
        )
        .expect("config");
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().expect("now");
    let cutoff = local_day_start(now, &TimeZone::UTC)
        .expect("cutoff")
        .as_second() as u64;
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path("scope"),
        &crate::agents::spending::WorkspaceSpendingCache {
            scope_hash: "scope".to_owned(),
            day: crate::agents::spending::SpendWindow {
                usd: 5.25,
                ..Default::default()
            },
            day_cutoff_secs: cutoff,
            ..Default::default()
        },
    );
    let kind = AgentKind::new_unchecked("claude");
    assert!(
        scope_gate(
            &runtime,
            &state_paths(&runtime),
            Some(&LoginKey::default_for(kind.clone())),
            &config,
            now
        )
        .is_some_and(|reason| reason.contains("fleet budget exhausted"))
    );
    let availability = crate::harness::plan::LaunchAvailability::read(
        &runtime,
        &state_paths(&runtime),
        &config,
        now,
    );
    assert!(
        availability.unavailable("claude", "opus").is_none(),
        "fleet spend never routes a tier"
    );

    let mut fleet = DailyBudgetScope::Fleet.read_ledger(&runtime, Some(&state_paths(&runtime)));
    fleet.disabled = true;
    DailyBudgetScope::Fleet
        .write_ledger(&runtime, &state_paths(&runtime), &fleet)
        .expect("disable fleet");
    let spending = crate::agents::spending::Spending::default();
    let provider_day = BTreeMap::from([(
        LoginKey::default_for(kind.clone()),
        crate::agents::spending::SpendWindow {
            usd: 10.5,
            ..Default::default()
        },
    )]);
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &crate::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: now.as_millisecond() as u64,
            spending,
            days: BTreeMap::new(),
            models: BTreeMap::new(),
            day_by_login: provider_day,
            day_cutoff_secs: cutoff,
            ..Default::default()
        },
    );
    assert!(
        scope_gate(
            &runtime,
            &state_paths(&runtime),
            Some(&LoginKey::default_for(kind)),
            &config,
            now
        )
        .is_some_and(|reason| reason.contains("claude@default account budget exhausted"))
    );
    let availability = crate::harness::plan::LaunchAvailability::read(
        &runtime,
        &state_paths(&runtime),
        &config,
        now,
    );
    assert!(matches!(
        availability.unavailable("claude", "opus"),
        Some(crate::agents::TierSkipReason::DailyCap { .. })
    ));
    assert!(availability.unavailable("codex", "gpt-6-astra").is_none());
    crate::disk::atomic::write_temp_then_rename_cache(
        &runtime.shared_rate_limits_path(),
        &crate::agents::account::RateLimitsCache {
            entries: [(
                LoginKey::default_for(AgentKind::new_unchecked("claude")),
                crate::agents::account::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits {
                        windows: vec![crate::agents::RateLimitWindow {
                            scope: Some(crate::agents::RateLimitWindowScope {
                                id: "model:opus".into(),
                                label: "Opus".into(),
                            }),
                            used_percentage: Some(100),
                            duration_mins: Some(300),
                            resets_at: Some(now + jiff::SignedDuration::from_hours(1)),
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    )
    .unwrap();
    let scoped = crate::harness::plan::LaunchAvailability::read(
        &runtime,
        &state_paths(&runtime),
        &config,
        now,
    );
    assert!(matches!(
        scoped.unavailable("claude", "opus"),
        Some(crate::agents::TierSkipReason::Exhausted { .. })
    ));
    assert!(
        scoped.unavailable("codex", "gpt-6-astra").is_none(),
        "Claude-only model windows never skip Codex"
    );
    let mut accounts = crate::agents::account::AccountsCache::default();
    let key = LoginKey::default_for(AgentKind::new_unchecked("codex"));
    accounts.logins.insert(
        key.clone(),
        crate::agents::account::ProviderRecord {
            login: None,
            probed_at_ms: 1,
            ok: false,
            account: None,
        },
    );
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.shared_accounts_path(), &accounts)
        .unwrap();
    let unknown = crate::harness::plan::LaunchAvailability::read(
        &runtime,
        &state_paths(&runtime),
        &config,
        now,
    );
    assert!(unknown.unavailable("codex", "gpt-6-astra").is_none());
    accounts.logins.get_mut(&key).unwrap().ok = true;
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.shared_accounts_path(), &accounts)
        .unwrap();
    assert!(
        unknown.unavailable("codex", "gpt-6-astra").is_none(),
        "one launch keeps one snapshot"
    );
    let logged_out = crate::harness::plan::LaunchAvailability::read(
        &runtime,
        &state_paths(&runtime),
        &config,
        now,
    );
    assert_eq!(
        logged_out.unavailable("codex", "gpt-6-astra"),
        Some(crate::agents::TierSkipReason::LoggedOut)
    );
    let mut parked =
        DailyBudgetScope::Account(key.clone()).read_ledger(&runtime, Some(&state_paths(&runtime)));
    parked.parked = Some(BudgetParkStamp {
        at: now,
        at_cost: 12.0,
    });
    let scope = DailyBudgetScope::Account(key);
    scope
        .write_ledger(&runtime, &state_paths(&runtime), &parked)
        .unwrap();
    let config: MachineConfig =
        toml::from_str("timezone = 'UTC'\n[accounts.budget]\ncodex = '10/day'").unwrap();
    assert_eq!(
        scope.exhausted(
            &runtime,
            &state_paths(&runtime),
            &config,
            now,
            &Default::default()
        ),
        Some((12.0, 10.0))
    );
    assert!(
        scope
            .exhausted(
                &runtime,
                &state_paths(&runtime),
                &config,
                now + jiff::SignedDuration::from_hours(24),
                &Default::default()
            )
            .is_none()
    );
}

#[test]
fn account_budget_isolates_logins_and_projects_the_room_account() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("dirs");
    let config: MachineConfig = toml::from_str(
        "timezone = \"UTC\"\n[accounts.budget]\nclaude = \"10/day\"\n[accounts.claude.work]\nhome = \"/srv/budget-test-work\"\nhistory = \"standalone\"\n",
    ).expect("config");
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().expect("now");
    let cutoff = local_day_cutoff_secs(now, &TimeZone::UTC).expect("cutoff");
    let default = agent(0.0, AgentStatus::Running, Some(now));
    let mut work = default.clone();
    work.agent_id = "work-session".into();
    work.login = Some("work".parse().expect("login"));
    let default_key = LoginKey::default_for(default.kind.clone());
    let work_key = LoginKey::new(work.kind.clone(), work.login.clone().expect("login"));
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id, vec![default, work], now)
        .with_provider_aggregates(
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &std::collections::BTreeSet::from([default_key.clone(), work_key.clone()]),
        );
    let mut provider = ProviderSpendingCache {
        day_cutoff_secs: cutoff,
        day_by_provider: BTreeMap::from([(
            "claude".to_owned(),
            SpendWindow {
                usd: 99.0,
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    write_provider_spending_cache(&runtime.shared_provider_spending_path(), &provider);
    let scopes = evaluate_scopes(
        &snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
        now,
        Some(cutoff),
    );
    assert_eq!(binding_scope_park(&scopes, &default_key), None);
    assert_eq!(
        binding_scope_park(&scopes, &work_key),
        None,
        "kind-wide spend cannot park a missing login"
    );

    provider.day_by_login = BTreeMap::from([
        (
            default_key.clone(),
            SpendWindow {
                usd: 2.0,
                ..Default::default()
            },
        ),
        (
            work_key.clone(),
            SpendWindow {
                usd: 12.0,
                ..Default::default()
            },
        ),
    ]);
    write_provider_spending_cache(&runtime.shared_provider_spending_path(), &provider);
    let scopes = evaluate_scopes(
        &snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
        now,
        Some(cutoff),
    );
    assert_eq!(scopes.daily.len(), 3);
    assert_eq!(binding_scope_park(&scopes, &default_key), None);
    assert_eq!(binding_scope_park(&scopes, &work_key), Some(now));
    enforce(&snapshot, &runtime, None, &config);
    let scope = DailyBudgetScope::Account(work_key.clone());
    assert_eq!(
        scope
            .ledger_path(&runtime, &state_paths(&runtime))
            .file_name()
            .unwrap(),
        "budget.account.claude@work.json"
    );
    assert!(
        scope
            .read_ledger(&runtime, Some(&state_paths(&runtime)))
            .parked
            .is_some()
    );
    assert!(
        !DailyBudgetScope::Account(default_key)
            .ledger_path(&runtime, &state_paths(&runtime))
            .exists()
    );
    project_parks(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
    );
    assert!(snapshot.agents[0].budget_park.is_none());
    assert_eq!(
        snapshot.agents[1]
            .budget_park
            .as_ref()
            .expect("work park")
            .scope,
        BudgetScope::Account
    );

    let logins = RoomLoginSet::new(
        Some(BTreeMap::from([(
            work_key.kind.clone(),
            work_key.name.clone(),
        )])),
        Some(crate::agents::LoginCatalog::from_config(&config.accounts).expect("catalog")),
        BTreeMap::new(),
    )
    .with_agents(&snapshot.agents);
    project_budget_views(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
        &provider,
        &logins,
    );
    let panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.login_key() == work_key)
        .expect("panel");
    let budget = panel.day_budget.as_ref().expect("budget");
    assert_eq!(budget.spend_usd, 12.0);
    assert!(budget.parked);
    let native = snapshot
        .providers
        .iter()
        .find(|panel| panel.account.is_default())
        .unwrap()
        .day_budget
        .as_ref()
        .unwrap();
    assert_eq!(native.spend_usd, 2.0);
    assert!(!native.parked);
    provider.day_by_login.clear();
    write_provider_spending_cache(&runtime.shared_provider_spending_path(), &provider);
    assert!(
        scope_gate(
            &runtime,
            &state_paths(&runtime),
            Some(&work_key),
            &config,
            now
        )
        .is_some()
    );
    project_parks(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
    );
    assert_eq!(
        snapshot.agents[1]
            .budget_park
            .as_ref()
            .expect("park survives missing spend")
            .spend_usd,
        12.0
    );
    let unresolved = RoomLoginSet::new(None, None, BTreeMap::new());
    project_budget_views(
        &mut snapshot,
        &runtime,
        Some(&state_paths(&runtime)),
        &config,
        &provider,
        &unresolved,
    );
    assert!(
        snapshot
            .providers
            .iter()
            .all(|panel| panel.day_budget.is_none())
    );
}
#[test]
fn accounts_of_one_pool_share_a_ledger_and_park_together() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("dirs");
    let config: MachineConfig = toml::from_str(
        "timezone = \"UTC\"\n[accounts.budget]\nclaude = \"10/day\"\n[accounts.claude.work]\nhome = \"/srv/budget-test-work\"\n[accounts.claude.personal]\nhome = \"/srv/budget-test-personal\"\n[accounts.claude.solo]\nhome = \"/srv/budget-test-solo\"\nhistory = \"standalone\"\n",
    ).expect("config");
    let catalog = crate::agents::LoginCatalog::from_config(&config.accounts).expect("catalog");
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().expect("now");
    let cutoff = local_day_cutoff_secs(now, &TimeZone::UTC).expect("cutoff");
    let on = |login: &str| {
        let mut agent = agent(0.0, AgentStatus::Running, Some(now));
        agent.agent_id = format!("{login}-session").into();
        agent.login = Some(login.parse().expect("login"));
        agent
    };
    let agents = vec![on("work"), on("personal"), on("solo")];
    let keys: Vec<_> = agents.iter().map(AgentState::login_key).collect();
    let pool = LoginKey::default_for(agents[0].kind.clone());
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id, agents, now);
    // The walk publishes by pool: the shared accounts' spend is the default's.
    let provider = ProviderSpendingCache {
        day_cutoff_secs: cutoff,
        day_by_login: BTreeMap::from([
            (
                pool.clone(),
                SpendWindow {
                    usd: 12.0,
                    ..Default::default()
                },
            ),
            (
                keys[2].clone(),
                SpendWindow {
                    usd: 2.0,
                    ..Default::default()
                },
            ),
        ]),
        ..Default::default()
    };
    write_provider_spending_cache(&runtime.shared_provider_spending_path(), &provider);
    let state = state_paths(&runtime);

    let scopes = evaluate_scopes(
        &snapshot,
        &runtime,
        Some(&state),
        &config,
        now,
        Some(cutoff),
    );
    assert_eq!(scopes.daily.len(), 3, "fleet, the pool, and the standalone");
    assert_eq!(binding_scope_park(&scopes, &keys[0]), Some(now));
    assert_eq!(binding_scope_park(&scopes, &keys[1]), Some(now));
    assert_eq!(binding_scope_park(&scopes, &keys[2]), None);

    enforce(&snapshot, &runtime, None, &config);
    let work = DailyBudgetScope::account(&catalog, &keys[0]);
    assert_eq!(work, DailyBudgetScope::account(&catalog, &keys[1]));
    assert_eq!(work, DailyBudgetScope::account(&catalog, &pool));
    assert_eq!(
        work.ledger_path(&runtime, &state).file_name().unwrap(),
        "budget.account.claude@default.json"
    );
    assert!(work.read_ledger(&runtime, Some(&state)).parked.is_some());
    let solo = DailyBudgetScope::account(&catalog, &keys[2]);
    assert_eq!(solo, DailyBudgetScope::Account(keys[2].clone()));
    assert!(!solo.ledger_path(&runtime, &state).exists());

    project_parks(&mut snapshot, &runtime, Some(&state), &config);
    let parked: Vec<_> = snapshot
        .agents
        .iter()
        .map(|agent| agent.budget_park.as_ref().map(|park| park.spend_usd))
        .collect();
    assert_eq!(parked, [Some(12.0), Some(12.0), None]);
    assert!(scope_gate(&runtime, &state, Some(&keys[1]), &config, now).is_some());
    assert!(scope_gate(&runtime, &state, Some(&keys[2]), &config, now).is_none());
}

fn state_paths(runtime: &RuntimePaths) -> crate::StatePaths {
    let home = runtime
        .root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    crate::StatePaths::under_named(runtime.workspace_id.clone(), runtime.dir_name.clone(), home)
}

/// Runtime paths whose ledgers and ledger locks live in different directories,
/// as in production.
fn split_shared_roots() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    runtime.persistent_shared_root = dir.path().join("providers");
    std::fs::create_dir_all(&runtime.persistent_shared_root).expect("providers dir");
    std::fs::create_dir_all(&runtime.shared_root).expect("shared dir");
    (dir, runtime)
}

fn seed_account_ledger(runtime: &RuntimePaths, component: &str) -> (PathBuf, PathBuf) {
    let ledger = runtime.shared_account_budget_ledger(component);
    let lock = runtime.shared_account_budget_lock(component);
    std::fs::write(&ledger, b"{}").expect("ledger");
    std::fs::write(&lock, b"").expect("lock");
    (ledger, lock)
}

fn accounts_config(accounts: &str) -> MachineConfig {
    toml::from_str(accounts).expect("config")
}

const SHARED_AND_STANDALONE: &str = "[accounts.claude.work]\nhome = \"/srv/budget-test-work\"\n[accounts.claude.solo]\nhome = \"/srv/budget-test-solo\"\nhistory = \"standalone\"\n";

#[test]
fn ledger_sweep_removes_only_ledgers_no_pool_reads() {
    let (_dir, runtime) = split_shared_roots();
    let (work, work_lock) = seed_account_ledger(&runtime, "claude@work");
    let (legacy, legacy_lock) = seed_account_ledger(&runtime, "codex");
    let kept: Vec<PathBuf> = ["claude@default", "claude@solo", "claude@gone"]
        .into_iter()
        .flat_map(|component| <[PathBuf; 2]>::from(seed_account_ledger(&runtime, component)))
        .chain(
            ["accounts.json", "auto_redeem_rate.codex@work.json"].map(|name| {
                let path = runtime.persistent_shared_root.join(name);
                std::fs::write(&path, b"{}").expect("neighbour");
                path
            }),
        )
        .collect();

    let sweep =
        sweep_orphan_account_ledgers(&runtime, &accounts_config(SHARED_AND_STANDALONE), false);

    assert_eq!(
        sweep,
        AccountLedgerSweep {
            ledgers: 2,
            bytes: 4
        }
    );
    for gone in [work, work_lock, legacy, legacy_lock] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    for path in kept {
        assert!(path.exists(), "{}", path.display());
    }
}

#[test]
fn ledger_sweep_removes_the_kind_only_ledger_without_named_accounts() {
    let (_dir, runtime) = split_shared_roots();
    let (legacy, legacy_lock) = seed_account_ledger(&runtime, "codex");
    let (default, _) = seed_account_ledger(&runtime, "codex@default");

    let sweep = sweep_orphan_account_ledgers(&runtime, &MachineConfig::default(), false);

    assert_eq!(sweep.ledgers, 1);
    assert!(!legacy.exists() && !legacy_lock.exists());
    assert!(default.exists());
}

#[test]
fn ledger_sweep_leaves_a_ledger_whose_lock_is_held() {
    let (_dir, runtime) = split_shared_roots();
    let (ledger, lock) = seed_account_ledger(&runtime, "claude@work");
    let config = accounts_config(SHARED_AND_STANDALONE);
    let held = crate::disk::lock::WorkspaceLock::acquire(&lock).expect("hold");

    let busy = sweep_orphan_account_ledgers(&runtime, &config, false);
    assert_eq!(busy, AccountLedgerSweep::default());
    assert!(ledger.exists() && lock.exists());

    drop(held);
    let free = sweep_orphan_account_ledgers(&runtime, &config, false);
    assert_eq!(free.ledgers, 1);
    assert!(!ledger.exists() && !lock.exists());
}

#[test]
fn ledger_sweep_keeps_named_ledgers_when_the_accounts_config_does_not_load() {
    let (_dir, runtime) = split_shared_roots();
    let (work, work_lock) = seed_account_ledger(&runtime, "claude@work");
    let config = accounts_config("[accounts.claude.work]\nhome = \"relative\"\n");
    assert!(crate::agents::LoginCatalog::from_config(&config.accounts).is_err());

    let sweep = sweep_orphan_account_ledgers(&runtime, &config, false);

    assert_eq!(sweep, AccountLedgerSweep::default());
    assert!(work.exists() && work_lock.exists());
}

#[test]
fn ledger_sweep_dry_run_counts_and_touches_nothing() {
    let (_dir, runtime) = split_shared_roots();
    let ledger = runtime.shared_account_budget_ledger("claude@work");
    std::fs::write(&ledger, b"{}").expect("ledger");

    let sweep =
        sweep_orphan_account_ledgers(&runtime, &accounts_config(SHARED_AND_STANDALONE), true);

    assert_eq!(
        sweep,
        AccountLedgerSweep {
            ledgers: 1,
            bytes: 2
        }
    );
    assert!(ledger.exists());
    assert!(!runtime.shared_account_budget_lock("claude@work").exists());
}

#[test]
fn ledger_sweep_unlinks_its_lock_when_the_ledger_unlink_fails() {
    let (_dir, runtime) = split_shared_roots();
    let ledger = runtime.shared_account_budget_ledger("codex");
    std::fs::create_dir(&ledger).expect("a directory under a ledger name");

    let sweep = sweep_orphan_account_ledgers(&runtime, &MachineConfig::default(), false);

    assert_eq!(sweep.ledgers, 0);
    assert!(ledger.exists());
    assert!(!runtime.shared_account_budget_lock("codex").exists());
}
