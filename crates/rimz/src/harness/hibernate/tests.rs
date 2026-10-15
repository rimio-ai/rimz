use super::*;

#[test]
fn schedule_blockers_offer_the_built_in_defaults() {
    for (after, deep_after) in [("invalid", "120m"), ("30m", "25h"), ("3h", "1h")] {
        let config = HibernateConfig {
            after: after.into(),
            deep_after: deep_after.into(),
            ..Default::default()
        };
        let blocker = crate::blocker::Blocker::from(config.schedule().unwrap_err());
        let defaults = HibernateConfig::default();
        for (key, value) in [
            ("after", defaults.after),
            ("deep_after", defaults.deep_after),
        ] {
            let command = format!("rimz config set agents.hibernate.{key} {value}");
            assert!(
                blocker.fixes.iter().any(|fix| {
                    fix.command.as_deref() == Some(command.as_str())
                        && fix.surface == crate::blocker::Surface::Plain
                }),
                "{blocker:?}"
            );
        }
    }
}

#[test]
fn focus_consumer_wakes_both_backends_and_paces_repeated_events() {
    use crate::wakeup::events::SidebarEvent;
    for raw in ["tmux:%3", "zellij:terminal_3"] {
        let pane: PaneId = raw.parse().unwrap();
        let entry = crate::store::hibernate::Entry {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "session".into(),
            pane_id: pane.clone(),
            scope: PathBuf::new(),
            provider_pid: 10,
            frozen_at: jiff::Timestamp::UNIX_EPOCH,
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 0,
        };
        let event = SidebarEvent::FocusChanged {
            focused: vec![pane.clone()],
            unfocused: vec![],
        };
        let entries = [entry];
        let mut pacing = std::collections::HashMap::new();
        assert_eq!(
            focus_wake_targets(&event, &entries, &pacing, 10_000),
            std::slice::from_ref(&pane)
        );
        pacing.insert(pane.clone(), 8_001);
        assert!(focus_wake_targets(&event, &entries, &pacing, 10_000).is_empty());
        pacing.insert(pane.clone(), 8_000);
        assert_eq!(
            focus_wake_targets(&event, &entries, &pacing, 10_000),
            std::slice::from_ref(&pane)
        );
        assert!(focus_wake_targets(&event, &[], &pacing, 10_000).is_empty());
        let unfocused = SidebarEvent::FocusChanged {
            focused: vec![],
            unfocused: vec![pane],
        };
        assert!(focus_wake_targets(&unfocused, &entries, &pacing, 10_000).is_empty());
        assert!(
            focus_wake_targets(&SidebarEvent::PanesChanged, &entries, &pacing, 10_000).is_empty()
        );
    }
}

fn decision_fixture() -> (tempfile::TempDir, Store, AgentState, Policy, ScopeView) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let paths = crate::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
    let runtime = crate::RuntimePaths::under(id.clone(), &dir.path().join("runtime")).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "session".into(),
        crate::agents::AgentStatus::Idle,
        jiff::Timestamp::from_second(1_000).unwrap(),
    );
    agent.turn_ended_at = Some(agent.last_activity);
    let mut config = MachineConfig::default();
    config.agents.hibernate.enabled = Some(true);
    config.agents.hibernate.after = "3m".into();
    let uid = nix::unistd::Uid::current().as_raw();
    let scope = ScopeView {
        scope: ScopePath::from_cgroup(
            dir.path(),
            &format!(
                "0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{}test.scope",
                scope_prefix(&id)
            ),
        ),
        provider_pid: 10,
        members: vec![10, 11, 12],
        parents: [
            (10, ("claude".into(), 1)),
            (11, ("node".into(), 10)),
            (12, ("mcp".into(), 11)),
        ]
        .into(),
        focused: false,
    };
    (
        dir,
        store,
        agent,
        Policy::new(&config, &KeepWarmPolicy::load(&config, None)).unwrap(),
        scope,
    )
}

#[test]
fn keep_warm_holds_hibernation_only_while_the_cache_can_stay_warm() {
    let (_dir, store, mut agent, _policy, scope) = decision_fixture();
    let mut config = MachineConfig::default();
    config.agents.hibernate.after = "3m".into();
    config.agents.profiles.0.insert(
        "warm".into(),
        toml::from_str("agent = 'claude'\nkeep-warm = '2h'").unwrap(),
    );
    let policy = Policy::new(&config, &KeepWarmPolicy::load(&config, None)).unwrap();
    agent.profile = Some("warm".into());
    agent.turn_started_at = Some(agent.last_activity);
    assert_eq!(
        decision(&store, &agent, &policy, &scope, 1_180),
        Verdict::Hold(Hold::KeepWarm)
    );
    assert_eq!(
        decision(&store, &agent, &policy, &scope, 8_200),
        Verdict::Hibernate { idle_secs: 7_200 }
    );
}

#[test]
fn sync_waking_restarts_grace_for_a_queued_helper_and_recorded_pane() {
    for recorded in [false, true] {
        let (dir, store, mut agent, config, scope) = decision_fixture();
        let pane: PaneId = "tmux:%3".parse().unwrap();
        agent.pane = Some(crate::pane::PaneRef::from_id(pane.clone()));
        let path = scope.scope.as_ref().unwrap().as_path();
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("cgroup.freeze"), "1").unwrap();
        drop(
            crate::disk::lock::WorkspaceLock::acquire(&store.paths().lock_path("hibernate.lock"))
                .unwrap(),
        );
        if recorded {
            crate::store::hibernate::record(
                store.paths(),
                crate::store::hibernate::Entry {
                    kind: agent.kind.clone(),
                    agent_id: agent.agent_id.clone(),
                    pane_id: pane.clone(),
                    scope: path.into(),
                    provider_pid: 10,
                    frozen_at: agent.last_activity,
                    anon_at_freeze: 0,
                    step: 0,
                    wake_error: None,
                    reclaimed_bytes: 0,
                },
            )
            .unwrap();
        }
        let now = jiff::Timestamp::now().as_second();
        super::wake::wake_pane_with_timeout(
            store.paths(),
            store.runtime_paths(),
            &pane,
            dir.path(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(
            decision(&store, &agent, &config, &scope, now),
            Verdict::Hold(Hold::WakeClock),
            "recorded={recorded}"
        );
        assert!(matches!(
            decision(&store, &agent, &config, &scope, now + 181),
            Verdict::Hibernate { .. }
        ));
    }
}

#[test]
fn focus_stamp_holds_since_wake_or_departure_but_a_turn_uses_the_rest_clock() {
    let (_dir, store, mut agent, config, scope) = decision_fixture();
    let pane: PaneId = "tmux:%3".parse().unwrap();
    agent.pane = Some(crate::pane::PaneRef::from_id(pane.clone()));
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(store.runtime_paths(), Some(&pane)),
        &serde_json::json!({"focus": "1970-01-01T00:20:00Z"}),
    )
    .unwrap();
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_209),
        Verdict::Hold(Hold::WakeClock)
    );
    assert!(matches!(
        decision(&store, &agent, &config, &scope, 1_210),
        Verdict::Hibernate { .. }
    ));
    crate::disk::atomic::write_temp_then_rename_cache(
        &store.runtime_paths().live_path("hibernate").join("focus.json"),
        &serde_json::json!({"focused": null, "departed": {pane.to_string(): "1970-01-01T00:21:40Z"}}),
    ).unwrap();
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_309),
        Verdict::Hold(Hold::WakeClock)
    );
    assert!(matches!(
        decision(&store, &agent, &config, &scope, 1_310),
        Verdict::Hibernate { .. }
    ));
    agent.last_activity = jiff::Timestamp::from_second(1_305).unwrap();
    agent.turn_ended_at = Some(agent.last_activity);
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_310),
        Verdict::Hold(Hold::Clock)
    );
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_484),
        Verdict::Hold(Hold::Clock)
    );
    assert!(matches!(
        decision(&store, &agent, &config, &scope, 1_485),
        Verdict::Hibernate { .. }
    ));
}

#[test]
fn bare_pane_and_workspace_stamps_hold_the_full_after() {
    for workspace in [false, true] {
        let (_dir, store, mut agent, config, scope) = decision_fixture();
        let pane: PaneId = "tmux:%3".parse().unwrap();
        agent.pane = Some(crate::pane::PaneRef::from_id(pane.clone()));
        crate::disk::atomic::write_temp_then_rename_cache(
            &wake_stamp_path(store.runtime_paths(), (!workspace).then_some(&pane)),
            &jiff::Timestamp::from_second(1_200).unwrap(),
        )
        .unwrap();
        assert_eq!(
            decision(&store, &agent, &config, &scope, 1_379),
            Verdict::Hold(Hold::WakeClock)
        );
        assert!(matches!(
            decision(&store, &agent, &config, &scope, 1_380),
            Verdict::Hibernate { .. }
        ));
    }
}

#[test]
fn provider_scope_descends_past_unscoped_exec_wrapper() {
    let (_dir, store, _agent, _policy, view) = decision_fixture();
    let workspace = &store.paths().workspace_id;
    let scope = view.scope.unwrap();
    // Process classification recognizes both `rimz agents exec claude` and Claude.
    let provider = |pid| matches!(pid, 10 | 20).then_some(pid);
    let scope_of = |pid| (pid == 20).then(|| scope.clone());
    let children = |pid| if pid == 10 { vec![20] } else { vec![] };
    assert_eq!(
        scoped_provider(10, workspace, &provider, &scope_of, &children),
        Some((20, scope.clone())),
        "an unscoped exec wrapper must not hide its scoped provider"
    );
    assert_eq!(
        scoped_provider(20, workspace, &provider, &scope_of, &children),
        Some((20, scope.clone()))
    );
    assert!(scoped_provider(10, workspace, &provider, &scope_of, &|_| vec![20, 30]).is_none());
    assert!(scoped_provider(10, workspace, &provider, &|_| None, &children).is_none());
}

#[test]
fn provider_scope_descends_past_a_scope_inherited_from_another_room() {
    let (dir, store, _agent, _policy, view) = decision_fixture();
    let workspace = &store.paths().workspace_id;
    let own = view.scope.unwrap();
    let uid = nix::unistd::Uid::current().as_raw();
    let other = WorkspaceId::from_project_root(&dir.path().join("other"));
    let inherited = ScopePath::from_cgroup(
        dir.path(),
        &format!(
            "0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{}outer.scope",
            scope_prefix(&other)
        ),
    )
    .unwrap();
    // The exec wrapper runs in the mux server's cgroup, inherited from the
    // agent pane that started this room's server.
    let provider = |pid| matches!(pid, 10 | 20).then_some(pid);
    let scope_of = |pid| match pid {
        10 => Some(inherited.clone()),
        20 => Some(own.clone()),
        _ => None,
    };
    assert_eq!(
        scoped_provider(10, workspace, &provider, &scope_of, &|pid| {
            if pid == 10 { vec![20] } else { vec![] }
        }),
        Some((20, own.clone())),
        "another room's agent scope must not hide the provider's own"
    );
    for children in [vec![], vec![20, 30]] {
        assert!(
            scoped_provider(10, workspace, &provider, &scope_of, &|_| children.clone()).is_none(),
            "a foreign scope is never returned"
        );
    }
}

fn decision(
    store: &Store,
    agent: &AgentState,
    config: &Policy,
    scope: &ScopeView,
    now: i64,
) -> Verdict {
    decide(
        store,
        agent,
        config,
        jiff::Timestamp::from_second(now).unwrap(),
        scope,
    )
    .unwrap()
}

#[test]
fn provider_ancestors_are_owned_but_their_other_children_are_foreign() {
    let (_dir, store, agent, config, mut scope) = decision_fixture();
    scope.members = vec![8, 9, 10, 11, 12];
    scope.parents.insert(8, ("bwrap".into(), 1));
    scope.parents.insert(9, ("sh".into(), 8));
    scope.parents.insert(10, ("claude".into(), 9));
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_180),
        Verdict::Hibernate { idle_secs: 180 }
    );
    scope.members.push(13);
    scope.parents.insert(13, ("foreign".into(), 9));
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_180),
        Verdict::Hold(Hold::ForeignProcess {
            pid: 13,
            comm: "foreign".into()
        })
    );
}

#[test]
fn decision_requires_unfocused_scope_and_provider_membership() {
    let (_dir, store, agent, config, clear) = decision_fixture();
    let ready = Verdict::Hibernate { idle_secs: 180 };
    assert_eq!(decision(&store, &agent, &config, &clear, 1_180), ready);
    for (expected, changed) in [
        (
            Hold::Focused,
            ScopeView {
                focused: true,
                ..clear.clone()
            },
        ),
        (
            Hold::NoScope,
            ScopeView {
                scope: None,
                ..clear.clone()
            },
        ),
        (
            Hold::NoScope,
            ScopeView {
                members: vec![],
                ..clear.clone()
            },
        ),
        (
            Hold::NoScope,
            ScopeView {
                members: vec![11, 12],
                ..clear.clone()
            },
        ),
    ] {
        assert_eq!(
            decision(&store, &agent, &config, &changed, 1_180),
            Verdict::Hold(expected)
        );
        assert_eq!(decision(&store, &agent, &config, &clear, 1_180), ready);
    }
    let mut foreign = clear.clone();
    foreign.parents.insert(12, ("server".into(), 1));
    assert_eq!(
        decision(&store, &agent, &config, &foreign, 1_180),
        Verdict::Hold(Hold::ForeignProcess {
            pid: 12,
            comm: "server".into()
        })
    );
    assert_eq!(
        decision(&store, &agent, &config, &clear, 1_179),
        Verdict::Hold(Hold::Clock)
    );
    let mut remote = config;
    remote.remote_control = toml::from_str("claude = true").unwrap();
    assert_eq!(
        decision(&store, &agent, &remote, &clear, 1_180),
        Verdict::Hold(Hold::RemoteControl)
    );
    remote.remote_control = RemoteControlConfig::default();
    assert_eq!(decision(&store, &agent, &remote, &clear, 1_180), ready);
}

#[test]
fn decision_rejects_each_busy_rollup_term_but_not_team_seats() {
    let (_dir, store, clear, config, scope) = decision_fixture();
    let ready = Verdict::Hibernate { idle_secs: 180 };
    assert_eq!(decision(&store, &clear, &config, &scope, 1_180), ready);
    type RollupMutation = fn(&mut AgentState);
    let cases: &[(Hold, RollupMutation)] = &[
        (Hold::Busy, |agent| {
            agent
                .background_shells
                .push(crate::agents::BackgroundShell {
                    id: "shell".into(),
                    command: None,
                    description: None,
                    started_at: agent.last_activity,
                })
        }),
        (Hold::Ended, |agent| {
            agent.ended_at = Some(agent.last_activity)
        }),
        (Hold::ProviderSubagent, |agent| {
            agent.parent_agent_id = Some("parent".into())
        }),
        (Hold::Busy, |agent| {
            agent.status = crate::agents::AgentStatus::Waiting
        }),
        (Hold::AwaitingInput, |agent| {
            agent.status = crate::agents::AgentStatus::Waiting;
            agent.waiting_since = Some(agent.last_activity);
        }),
        (Hold::Compacting, |agent| {
            agent.compacting_since = Some(agent.last_activity)
        }),
        (Hold::Busy, |agent| {
            agent.status = crate::agents::AgentStatus::Running
        }),
        (Hold::Busy, |agent| {
            agent.status = crate::agents::AgentStatus::Failed
        }),
        (Hold::Busy, |agent| {
            agent.phase = crate::agents::TurnPhase::Parked
        }),
        (Hold::BudgetPark, |agent| {
            agent.budget_park = Some(crate::agents::BudgetPark {
                cap_usd: 1.0,
                spend_usd: 1.0,
                window: crate::agents::BudgetWindow::Session,
                at: agent.last_activity,
                scope: crate::agents::BudgetScope::Agent,
                account_kind: None,
                resets_at: None,
            })
        }),
    ];
    for (expected, mutate) in cases {
        let mut agent = clear.clone();
        mutate(&mut agent);
        assert_eq!(
            decision(&store, &agent, &config, &scope, 1_180),
            Verdict::Hold(expected.clone())
        );
        assert_eq!(decision(&store, &clear, &config, &scope, 1_180), ready);
    }
    let mut seat = clear.clone();
    seat.team = Some("forge".into());
    assert_eq!(decision(&store, &seat, &config, &scope, 1_180), ready);
}

#[test]
fn messages_and_wakes_hold_only_until_terminal() {
    use crate::store::message::{
        DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
    };
    for wake in [false, true] {
        for status in [MessageStatus::Queued, MessageStatus::Delivered] {
            let (_dir, store, agent, config, scope) = decision_fixture();
            let observation = crate::agents::AgentLifecycleObservation::new(
                Some(agent.agent_id.clone()),
                crate::agents::LifecycleSignal::Registered,
            );
            store
                .append_event(&crate::store::event::EventEnvelope::agent_lifecycle(
                    store.paths().workspace_id.clone(),
                    "hibernate-test",
                    "claude",
                    "test",
                    &observation,
                ))
                .unwrap();
            let mut message = MessageRecord::new(
                store.paths().workspace_id.clone(),
                &agent,
                "hello".into(),
                DeliveryGate::Done,
            );
            if wake {
                message = message.with_sender(MessageSender::Harness {
                    notice: HarnessNotice::Wait,
                });
            }
            message.status = status;
            store.queue_message(&message, "hibernate-test").unwrap();
            let expected = if status.is_terminal() {
                Verdict::Hibernate { idle_secs: 180 }
            } else if wake {
                Verdict::Hold(Hold::Owed(super::super::owed::OwedWake::WakeInFlight))
            } else {
                Verdict::Hold(Hold::Message)
            };
            assert_eq!(decision(&store, &agent, &config, &scope, 1_180), expected);
        }
    }
}

#[test]
fn open_runs_hold_only_until_terminal() {
    use crate::store::run::{RunRecord, RunStatus};
    for status in [RunStatus::Running, RunStatus::Completed] {
        let (dir, store, agent, config, scope) = decision_fixture();
        let mut run = RunRecord::new(
            store.paths().workspace_id.clone(),
            agent.kind.clone(),
            crate::agents::PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        run.agent_id = Some(agent.agent_id.clone());
        run.status = status;
        super::super::run::create(store.paths(), &run).unwrap();
        let expected = if status.is_terminal() {
            Verdict::Hibernate { idle_secs: 180 }
        } else {
            Verdict::Hold(Hold::OpenRun)
        };
        assert_eq!(decision(&store, &agent, &config, &scope, 1_180), expected);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn producer_requires_unfocused_due_pane_and_paces_without_recording() {
    let (_dir, store, agent, _policy, _scope) = decision_fixture();
    let paths = store.paths();
    let runtime = store.runtime_paths();
    let mut config = MachineConfig::default();
    config.agents.hibernate.enabled = Some(true);
    config.agents.hibernate.after = "3m".into();
    let mut snapshot = crate::store::snapshot::SidebarSnapshot::build_with_agents(
        paths.workspace_id.clone(),
        vec![agent.clone()],
        jiff::Timestamp::from_second(1_180).unwrap(),
    );
    let pane: PaneId = "tmux:%3".parse().unwrap();
    std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(crate::store::snapshot::PaneAgent {
        root_lane: false,
        kind: agent.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(agent.agent_id.clone()),
        pane_id: pane.clone(),
        pane_pid: Some(42),
        worktree_path: None,
        worktree_branch: None,
    });
    let spawned = |snapshot: &crate::store::snapshot::SidebarSnapshot| {
        let mut requests = Vec::new();
        hibernate_idle_agents_with(
            snapshot,
            paths,
            runtime,
            &config,
            &KeepWarmPolicy::default(),
            |request| {
                requests.push(request.clone());
                true
            },
            |_| {},
        );
        requests
    };
    assert!(
        spawned(&snapshot).is_empty(),
        "missing admission must not spawn a freezer"
    );
    let path = runtime.live_path("hibernate").join("admission.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "invalid").unwrap();
    assert!(
        spawned(&snapshot).is_empty(),
        "unreadable admission must not spawn a freezer"
    );
    crate::disk::atomic::write_temp_then_rename_cache(
        &path,
        &Admission {
            available: true,
            reason: None,
        },
    )
    .unwrap();
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(runtime, Some(&pane)),
        &serde_json::json!({"focus": "1970-01-01T00:19:35Z"}),
    )
    .unwrap();
    assert!(spawned(&snapshot).is_empty());
    assert!(!fire_record_path(runtime, &agent.kind, &agent.agent_id).exists());
    snapshot.now = jiff::Timestamp::from_second(1_184).unwrap();
    assert!(spawned(&snapshot).is_empty());
    assert!(!fire_record_path(runtime, &agent.kind, &agent.agent_id).exists());
    snapshot.now = jiff::Timestamp::from_second(1_185).unwrap();
    assert_eq!(spawned(&snapshot).len(), 1);
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(runtime, Some(&pane)),
        &serde_json::json!({"focus": "1970-01-01T00:19:50Z"}),
    )
    .unwrap();
    snapshot.now = jiff::Timestamp::from_second(1_199).unwrap();
    assert!(spawned(&snapshot).is_empty());
    assert_eq!(
        read_fire_record(&fire_record_path(runtime, &agent.kind, &agent.agent_id))
            .fired_at
            .unwrap()
            .as_second(),
        1_185
    );
    snapshot.now = jiff::Timestamp::from_second(1_200).unwrap();
    assert_eq!(
        spawned(&snapshot).len(),
        1,
        "a fire before a focus wake must not extend its grace"
    );
    snapshot.now = jiff::Timestamp::from_second(1_230).unwrap();
    snapshot.focused_pane = Some(pane.clone());
    assert!(spawned(&snapshot).is_empty());
    snapshot.focused_pane = None;
    assert!(
        spawned(&snapshot).is_empty(),
        "leaving starts the focus grace"
    );
    snapshot.now = jiff::Timestamp::from_second(1_240).unwrap();
    let requests = spawned(&snapshot);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].pane_id, pane);
    assert_eq!(requests[0].root_pid, 42);
    assert_eq!(requests[0].idle_secs, 240);
    assert!(spawned(&snapshot).is_empty());
    snapshot.now = jiff::Timestamp::from_second(1_270).unwrap();
    assert_eq!(spawned(&snapshot).len(), 1);
    assert!(crate::store::hibernate::read(paths).is_empty());
    crate::store::hibernate::record(
        paths,
        crate::store::hibernate::Entry {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id: pane,
            scope: PathBuf::new(),
            provider_pid: 42,
            frozen_at: snapshot.now,
            anon_at_freeze: 600,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 300,
        },
    )
    .unwrap();
    std::sync::Arc::make_mut(&mut snapshot.agents)[0].status = crate::agents::AgentStatus::Sleeping;
    snapshot.now = jiff::Timestamp::from_second(3069).unwrap();
    assert!(spawned(&snapshot).is_empty());
    snapshot.now = jiff::Timestamp::from_second(3070).unwrap();
    assert_eq!(
        spawned(&snapshot).len(),
        1,
        "recorded agents deepen even with a sleeping status"
    );
    assert!(
        spawned(&snapshot).is_empty(),
        "deepening shares spawn pacing"
    );
}

#[test]
fn decision_does_not_refreeze_a_recorded_session() {
    let (_dir, store, agent, config, scope) = decision_fixture();
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_180),
        Verdict::Hibernate { idle_secs: 180 }
    );
    crate::store::hibernate::record(
        store.paths(),
        crate::store::hibernate::Entry {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id: "tmux:%3".parse().unwrap(),
            scope: scope.scope.as_ref().unwrap().as_path().into(),
            provider_pid: 10,
            frozen_at: agent.last_activity,
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 100,
        },
    )
    .unwrap();
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_180),
        Verdict::Hold(Hold::AlreadyHibernating)
    );
    assert_eq!(
        decision(&store, &agent, &config, &scope, 2_799),
        Verdict::Hold(Hold::AlreadyHibernating)
    );
    assert_eq!(
        decision(&store, &agent, &config, &scope, 2_800),
        Verdict::Deepen {
            step: 1,
            idle_secs: 1800
        }
    );
    crate::store::hibernate::withdraw(store.paths(), &agent.kind, &agent.agent_id).unwrap();
    assert_eq!(
        decision(&store, &agent, &config, &scope, 1_180),
        Verdict::Hibernate { idle_secs: 180 }
    );
}

#[test]
#[cfg(target_os = "linux")]
fn producer_repairs_lost_focus_events_and_focus_during_freeze() {
    for raw in ["tmux:%3", "zellij:terminal_3"] {
        let (_dir, store, agent, _policy, scope) = decision_fixture();
        let mut config = MachineConfig::default();
        config.agents.hibernate.enabled = Some(true);
        let pane: PaneId = raw.parse().unwrap();
        let mut snapshot = crate::store::snapshot::SidebarSnapshot::build_with_agents(
            store.paths().workspace_id.clone(),
            vec![agent.clone()],
            jiff::Timestamp::from_second(1_180).unwrap(),
        );
        snapshot.focused_pane = Some(pane.clone());
        let mut wakes = Vec::new();
        let mut produce = |snapshot: &crate::store::snapshot::SidebarSnapshot| {
            hibernate_idle_agents_with(
                snapshot,
                store.paths(),
                store.runtime_paths(),
                &config,
                &KeepWarmPolicy::default(),
                |_| panic!("focused pane must not freeze"),
                |pane| wakes.push(pane.clone()),
            );
        };
        produce(&snapshot);
        crate::store::hibernate::record(
            store.paths(),
            crate::store::hibernate::Entry {
                kind: agent.kind,
                agent_id: agent.agent_id,
                pane_id: pane.clone(),
                scope: scope.scope.unwrap().as_path().into(),
                provider_pid: 10,
                frozen_at: snapshot.now,
                anon_at_freeze: 0,
                step: 0,
                wake_error: None,
                reclaimed_bytes: 0,
            },
        )
        .unwrap();
        produce(&snapshot);
        produce(&snapshot);
        snapshot.now = jiff::Timestamp::from_second(1_181).unwrap();
        produce(&snapshot);
        snapshot.now = jiff::Timestamp::from_second(1_182).unwrap();
        produce(&snapshot);
        config.agents.hibernate.enabled = Some(false);
        snapshot.now = jiff::Timestamp::from_second(1_184).unwrap();
        hibernate_idle_agents_with(
            &snapshot,
            store.paths(),
            store.runtime_paths(),
            &config,
            &KeepWarmPolicy::default(),
            |_| panic!("disabled hibernation must not freeze"),
            |pane| wakes.push(pane.clone()),
        );
        assert_eq!(
            wakes,
            [pane.clone(), pane.clone(), pane.clone()],
            "pulled focus repairs missing events and late records every two seconds"
        );
        snapshot.focused_pane = None;
        snapshot.now = jiff::Timestamp::from_second(1_185).unwrap();
        hibernate_idle_agents_with(
            &snapshot,
            store.paths(),
            store.runtime_paths(),
            &config,
            &KeepWarmPolicy::default(),
            |_| panic!("disabled"),
            |_| panic!("unfocused pane needs no wake"),
        );
        let trail = read_focus_trail(store.runtime_paths());
        assert_eq!(trail.departed.get(&pane), Some(&snapshot.now));
    }
}

#[test]
fn producer_trails_departures_only_on_focus_changes_even_when_disabled() {
    for raw in ["tmux:%3", "zellij:terminal_3"] {
        let (_dir, store, agent, _policy, _scope) = decision_fixture();
        let pane: PaneId = raw.parse().unwrap();
        let mut config = MachineConfig::default();
        config.agents.hibernate.enabled = Some(false);
        let mut snapshot = crate::store::snapshot::SidebarSnapshot::build_with_agents(
            store.paths().workspace_id.clone(),
            vec![agent],
            jiff::Timestamp::from_second(1_180).unwrap(),
        );
        let produce = |snapshot: &crate::store::snapshot::SidebarSnapshot| {
            hibernate_idle_agents_with(
                snapshot,
                store.paths(),
                store.runtime_paths(),
                &config,
                &KeepWarmPolicy::default(),
                |_| panic!("disabled"),
                |_| {},
            );
        };
        snapshot.focused_pane = Some(pane.clone());
        produce(&snapshot);
        let path = store
            .runtime_paths()
            .live_path("hibernate")
            .join("focus.json");
        let first =
            std::fs::read(&path).expect("producer must publish focus trail before admission");
        snapshot.now = jiff::Timestamp::from_second(1_190).unwrap();
        produce(&snapshot);
        assert_eq!(std::fs::read(&path).unwrap(), first);
        snapshot.focused_pane = None;
        produce(&snapshot);
        let trail: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(trail["departed"][pane.to_string()], "1970-01-01T00:19:50Z");
    }
}

#[test]
fn record_projection_clears_stale_state_and_preserves_real_status() {
    let (_dir, store, agent, _config, scope) = decision_fixture();
    let mut snapshot = crate::store::snapshot::SidebarSnapshot::build_with_agents(
        store.paths().workspace_id.clone(),
        vec![agent.clone()],
        agent.last_activity,
    );
    crate::store::hibernate::record(
        store.paths(),
        crate::store::hibernate::Entry {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id: "tmux:%3".parse().unwrap(),
            scope: scope.scope.unwrap().as_path().into(),
            provider_pid: 10,
            frozen_at: agent.last_activity,
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 200,
        },
    )
    .unwrap();
    project_hibernation(&mut snapshot, store.paths());
    assert_eq!(
        snapshot.agents[0].effective_status(),
        crate::agents::AgentStatus::Idle
    );
    assert_eq!(
        snapshot.agents[0]
            .hibernation
            .as_ref()
            .unwrap()
            .reclaimed_bytes,
        200
    );
    std::sync::Arc::make_mut(&mut snapshot.agents)[0].status = crate::agents::AgentStatus::Running;
    project_hibernation(&mut snapshot, store.paths());
    assert_eq!(
        snapshot.agents[0].effective_status(),
        crate::agents::AgentStatus::Running
    );
    crate::store::hibernate::withdraw(store.paths(), &agent.kind, &agent.agent_id).unwrap();
    project_hibernation(&mut snapshot, store.paths());
    assert!(snapshot.agents[0].hibernation.is_none());
}

#[cfg(target_os = "linux")]
struct FakeHost {
    root: tempfile::TempDir,
    missing: Option<&'static str>,
}

#[cfg(target_os = "linux")]
impl FakeHost {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let slice = root.path().join("app.slice");
        std::fs::create_dir(&slice).unwrap();
        std::fs::write(slice.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        std::fs::write(slice.join("cgroup.freeze"), "0\n").unwrap();
        std::fs::write(slice.join("memory.reclaim"), "").unwrap();
        std::fs::write(slice.join("memory.swap.max"), "max\n").unwrap();
        std::fs::write(root.path().join("memory.swap.max"), "max\n").unwrap();
        Self {
            root,
            missing: None,
        }
    }
}

#[cfg(target_os = "linux")]
impl Host for FakeHost {
    fn systemd_run(&self) -> bool {
        self.missing != Some("systemd-run")
    }
    fn manager(&self) -> bool {
        self.missing != Some("user manager")
    }
    fn app_slice(&self) -> Result<PathBuf, String> {
        Ok(self.root.path().join("app.slice"))
    }
    fn root(&self) -> &Path {
        self.root.path()
    }
    fn writable(&self, path: &Path) -> bool {
        path.exists()
            && self.missing != path.file_name().and_then(|name| name.to_str())
            && !(self.missing == Some("memory controller") && path.is_dir())
    }
    fn read(&self, path: &Path) -> std::io::Result<String> {
        if path == Path::new("/proc/meminfo") {
            let swap = if self.missing == Some("swap") { 0 } else { 100 };
            return Ok(format!(
                "MemTotal: 100 kB\nMemAvailable: 50 kB\nSwapTotal: {swap} kB\nSwapFree: 20 kB\n"
            ));
        }
        std::fs::read_to_string(path)
    }
}

#[test]
#[cfg(target_os = "linux")]
fn preflight_checks_each_precondition_and_ends_errors_with_fixes() {
    let config = HibernateConfig {
        enabled: Some(true),
        ..Default::default()
    };
    for (name, fix) in [
        (
            "systemd-run",
            "install systemd or set agents.hibernate.enabled = false",
        ),
        (
            "user manager",
            "run loginctl enable-linger <user> or log in through a systemd session, or, when XDG_RUNTIME_DIR is unset or overridden (an SSH login, a sandbox), export XDG_RUNTIME_DIR=/run/user/$(id -u)",
        ),
        (
            "memory controller",
            "enable delegation with systemctl edit user@.service and Delegate=memory pids cpu",
        ),
        (
            "swap",
            "add zram, zswap or a swapfile; in a container, allow swap",
        ),
        ("swap limit", "raise the ancestor memory.swap.max limit"),
        (
            "cgroup.freeze",
            "enable writable cgroup-v2 freezer delegation or set agents.hibernate.enabled = false",
        ),
        (
            "memory.reclaim",
            "use Linux 5.19 or newer with writable memory controller delegation or set agents.hibernate.enabled = false",
        ),
    ] {
        let mut host = FakeHost::new();
        host.missing = Some(name);
        if name == "swap limit" {
            std::fs::write(host.root.path().join("memory.swap.max"), "0\n").unwrap();
        }
        let report = inspect_with(&config, &host);
        let failures: Vec<_> = report
            .checks
            .iter()
            .filter(|check| matches!(check.status, CheckStatus::Warning))
            .collect();
        assert_eq!(failures.len(), 1, "{name}: {report:?}");
        assert_eq!(failures[0].name, name);
        assert!(failures[0].detail.ends_with(fix), "{report:?}");
    }
    let host = FakeHost::new();
    let report = inspect_with(&config, &host);
    assert_eq!(report.checks.len(), 7);
    assert!(
        report
            .checks
            .iter()
            .all(|check| matches!(check.status, CheckStatus::Ok))
    );
}

#[test]
fn disabled_preflight_does_not_probe() {
    struct NoHost;
    impl Host for NoHost {
        fn systemd_run(&self) -> bool {
            panic!("disabled probe")
        }
        fn manager(&self) -> bool {
            panic!("disabled probe")
        }
        fn app_slice(&self) -> Result<PathBuf, String> {
            panic!("disabled probe")
        }
        fn read(&self, _: &Path) -> std::io::Result<String> {
            panic!("disabled probe")
        }
        fn writable(&self, _: &Path) -> bool {
            panic!("disabled probe")
        }
        fn root(&self) -> &Path {
            panic!("disabled probe")
        }
    }
    let report = inspect_with(
        &HibernateConfig {
            enabled: Some(false),
            ..Default::default()
        },
        &NoHost,
    );
    assert!(report.checks.is_empty());
    assert_eq!(
        report.skipped.as_deref(),
        Some(if cfg!(target_os = "linux") {
            "disabled"
        } else {
            "Linux cgroup freezing is unavailable"
        })
    );
}

#[test]
#[cfg(target_os = "linux")]
fn admission_publishes_capability_verdict_and_rejects_bad_schedule() {
    let (_dir, store, _, _, _) = decision_fixture();
    let runtime = store.runtime_paths();
    let mut host = FakeHost::new();
    let mut config = HibernateConfig::default();
    assert!(!Admission::read(runtime).available);
    let admitted = admit_with(&config, &host).unwrap();
    assert!(admitted.available);
    assert!(
        !runtime
            .live_path("hibernate")
            .join("admission.json")
            .exists(),
        "probing must not publish before birth"
    );
    admitted.publish(runtime).unwrap();
    assert!(Admission::read(runtime).available);
    host.missing = Some("swap");
    let skipped = admit_with(&config, &host).unwrap();
    skipped.publish(runtime).unwrap();
    assert!(!skipped.available);
    assert!(
        skipped
            .reason
            .as_deref()
            .unwrap()
            .contains("swap is absent")
    );
    assert_eq!(Admission::read(runtime).reason, skipped.reason);
    config.enabled = Some(false);
    assert_eq!(
        admit_with(&config, &host).unwrap().reason.as_deref(),
        Some("disabled")
    );
    config.after = "bad".into();
    assert!(admit_with(&config, &host).is_err());
    let report = inspect_with(&config, &host);
    assert!(matches!(report.checks[0].status, CheckStatus::Error));
}

#[test]
fn scope_argv_preserves_inner_command_and_names_workspace() {
    let workspace = WorkspaceId::from_project_root(Path::new("/project"));
    let argv = vec!["/bin/bwrap".into(), "--".into(), "provider".into()];
    let wrapped = scope_argv(&workspace, "claude", &argv);
    assert_eq!(wrapped.len(), 12 + argv.len());
    assert_eq!(
        &wrapped[..8],
        [
            "systemd-run",
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "-p",
            "MemoryAccounting=yes",
            "--unit"
        ]
    );
    assert!(wrapped[8].starts_with(&scope_prefix(&workspace)));
    assert!(wrapped[8].ends_with(".scope"));
    assert!(
        wrapped[8]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    );
    assert_eq!(
        &wrapped[9..12],
        ["--description", "RimZ agent claude", "--"]
    );
    assert_eq!(&wrapped[12..], argv);
    assert_ne!(wrapped[8], scope_argv(&workspace, "claude", &argv)[8]);
}
