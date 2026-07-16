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
        ..crate::store::agent_context::empty_context("test", now)
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
fn agent_digest_preserves_existing_ledger_names() {
    assert_eq!(
        agent_digest(&AgentKind::new_unchecked("claude"), &"sess".into()),
        "4a8d94f232e55a6a0879ba0858b59241"
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

    assert_eq!(
        spend_summary(&runtime, &state, Some(100.0)).as_deref(),
        Some("$5.25 of $6.00/day"),
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
    assert_eq!(
        spend_summary(&runtime, &state, None).as_deref(),
        Some("$4.50 of $4.00/day")
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
    project_parks(&mut snapshot, &runtime, &MachineConfig::default());
    assert!(snapshot.agents[0].budget_park.is_none());

    running.status = AgentStatus::Idle;
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![running],
        Timestamp::from_second(203).expect("timestamp"),
    );
    project_parks(&mut snapshot, &runtime, &MachineConfig::default());
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
        .write_ledger(
            &runtime,
            &DailyBudgetLedger {
                parked: Some(BudgetParkStamp {
                    at_cost: 6.0,
                    at: now,
                }),
                ..Default::default()
            },
        )
        .expect("fleet ledger");

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
    project_parks(&mut snapshot, &runtime, &config);
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
    let messages_dir = dir.path().join("messages");

    enforce(&snapshot, &runtime, &messages_dir, &config);
    assert!(!crate::harness::auto_continue::budget_park_armed(
        &runtime,
        &idle.kind,
        &idle.agent_id,
    ));

    let mut running = idle;
    running.status = AgentStatus::Running;
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id, vec![running.clone()], now);
    snapshot.fleet_day_spend_usd = Some(6.0);
    snapshot.fleet_day_spend_epoch_secs = Some(cutoff);
    snapshot.agent_panes = vec![crate::PaneAgent {
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

    enforce(&snapshot, &runtime, &messages_dir, &config);
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
        true,
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
        kind: state.kind,
        name: None,
        profile: None,
        role: None,
        channel: None,
    };
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
        dir.path(),
    )
    .expect("runtime");
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
        fleet_scope.ledger_path(&runtime),
        runtime.root.join("budget.fleet.json")
    );
    fleet_scope
        .write_ledger(&runtime, &fleet)
        .expect("fleet write");
    assert_eq!(fleet_scope.read_ledger(&runtime), fleet);
    fleet_scope
        .merge_park(&runtime, None)
        .expect("merge fleet park");
    assert_eq!(
        fleet_scope.read_ledger(&runtime),
        DailyBudgetLedger {
            parked: None,
            ..fleet.clone()
        },
        "producer park writes preserve CLI cap overrides"
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
    let account_scope = DailyBudgetScope::Account(kind.clone());
    assert_eq!(
        account_scope.ledger_path(&runtime),
        runtime
            .persistent_shared_root
            .join("budget.account.claude.json")
    );
    account_scope
        .write_ledger(&runtime, &account)
        .expect("account write");
    assert!(
        !std::fs::read_to_string(account_scope.ledger_path(&runtime))
            .expect("account ledger json")
            .contains("override_spec")
    );
    assert_eq!(account_scope.read_ledger(&runtime), account);
    account_scope
        .merge_park(&runtime, None)
        .expect("merge account park");
    assert_eq!(
        account_scope.read_ledger(&runtime),
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
            scope: BudgetScope::Account,
            account_kind: Some(kind),
            ..fleet_label
        }
        .label(),
        "claude account budget: $25.50 of $25.00/day"
    );
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
    let account_scope = DailyBudgetScope::Account(kind.clone());
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
    let scope = DailyBudgetScope::Account(kind);
    assert_eq!(scope.effective_cap_usd(&ledger, &config), None);
    assert_eq!(scope.cap_source(&ledger, &config), BudgetCapSource::None);
}

#[test]
fn park_projection_uses_agent_then_fleet_then_account_precedence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("dirs");
    let config: MachineConfig = toml::from_str(
            "timezone = \"UTC\"\n[harness]\nbudget = \"10/day\"\n[accounts.budget]\nclaude = \"20/day\"\n",
        )
        .expect("config");
    let now = Timestamp::from_second(200).expect("timestamp");
    let state = agent(6.0, AgentStatus::Idle, Some(now));
    let parked = BudgetParkStamp {
        at_cost: 30.0,
        at: now,
    };

    let mut agent_ledger = BudgetLedger::new("5".parse().expect("spec"));
    agent_ledger.parked = Some(parked.clone());
    agent_ledger.last_interrupt_at = Some(now);
    write_ledger(&runtime, &state.kind, &state.agent_id, &agent_ledger).expect("agent ledger");
    DailyBudgetScope::Fleet
        .write_ledger(
            &runtime,
            &DailyBudgetLedger {
                parked: Some(parked.clone()),
                ..Default::default()
            },
        )
        .expect("fleet ledger");
    DailyBudgetScope::Account(state.kind.clone())
        .write_ledger(
            &runtime,
            &DailyBudgetLedger {
                parked: Some(parked),
                ..Default::default()
            },
        )
        .expect("account ledger");
    write_scope_state(
        &runtime,
        &BudgetScopeState {
            last_interrupt_at: BTreeMap::from([(scope_agent_key(&state), now)]),
            ..Default::default()
        },
    )
    .expect("scope state");

    let projected_scope = |state: &AgentState| {
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id.clone(), vec![state.clone()], now);
        project_parks(&mut snapshot, &runtime, &config);
        snapshot.agents[0]
            .budget_park
            .as_ref()
            .map(|park| park.scope)
    };
    assert_eq!(projected_scope(&state), Some(BudgetScope::Agent));

    std::fs::remove_file(budget_ledger_path(&runtime, &state.kind, &state.agent_id))
        .expect("remove agent ledger");
    assert_eq!(projected_scope(&state), Some(BudgetScope::Fleet));

    let mut fleet = DailyBudgetScope::Fleet.read_ledger(&runtime);
    fleet.disabled = true;
    DailyBudgetScope::Fleet
        .write_ledger(&runtime, &fleet)
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
        scope_gate(&runtime, &kind, &config, now)
            .is_some_and(|reason| reason.contains("fleet budget exhausted"))
    );

    let mut fleet = DailyBudgetScope::Fleet.read_ledger(&runtime);
    fleet.disabled = true;
    DailyBudgetScope::Fleet
        .write_ledger(&runtime, &fleet)
        .expect("disable fleet");
    let spending = crate::agents::spending::Spending::default();
    let provider_day = BTreeMap::from([(
        "claude".to_owned(),
        crate::agents::spending::SpendWindow {
            usd: 10.5,
            ..Default::default()
        },
    )]);
    crate::agents::spending::write_provider_spending_cache_with_day(
        &runtime.shared_provider_spending_path(),
        now.as_millisecond() as u64,
        &spending,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &provider_day,
        cutoff,
    );
    assert!(
        scope_gate(&runtime, &kind, &config, now)
            .is_some_and(|reason| reason.contains("claude account budget exhausted"))
    );
}
