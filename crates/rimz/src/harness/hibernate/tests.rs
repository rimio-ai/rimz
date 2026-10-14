use super::*;

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
            last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
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
        pacing.insert(pane.clone(), 5_001);
        assert!(focus_wake_targets(&event, &entries, &pacing, 10_000).is_empty());
        pacing.insert(pane.clone(), 5_000);
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
fn waking_restarts_grace_for_a_queued_helper_and_after_a_glance() {
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
                    last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
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
            Verdict::Hold(Hold::Clock),
            "recorded={recorded}"
        );
        assert!(matches!(
            decision(&store, &agent, &config, &scope, now + 181),
            Verdict::Hibernate { .. }
        ));
    }
}

#[test]
fn provider_scope_descends_past_unscoped_exec_wrapper() {
    let (_dir, _store, _agent, _policy, view) = decision_fixture();
    let scope = view.scope.unwrap();
    // Process classification recognizes both `rimz agents exec claude` and Claude.
    let provider = |pid| matches!(pid, 10 | 20).then_some(pid);
    let scope_of = |pid| (pid == 20).then(|| scope.clone());
    let children = |pid| if pid == 10 { vec![20] } else { vec![] };
    assert_eq!(
        scoped_provider(10, &provider, &scope_of, &children),
        Some((20, scope.clone())),
        "an unscoped exec wrapper must not hide its scoped provider"
    );
    assert_eq!(
        scoped_provider(20, &provider, &scope_of, &children),
        Some((20, scope.clone()))
    );
    assert!(scoped_provider(10, &provider, &scope_of, &|_| vec![20, 30]).is_none());
    assert!(scoped_provider(10, &provider, &|_| None, &children).is_none());
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
fn decision_requires_unfocused_owned_scope_and_provider_membership() {
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
    assert_eq!(spawned(&snapshot).len(), 1);
    snapshot.now = jiff::Timestamp::from_second(1_210).unwrap();
    snapshot.focused_pane = Some(pane.clone());
    assert!(spawned(&snapshot).is_empty());
    snapshot.focused_pane = None;
    let requests = spawned(&snapshot);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].pane_id, pane);
    assert_eq!(requests[0].root_pid, 42);
    assert_eq!(requests[0].idle_secs, 210);
    assert!(spawned(&snapshot).is_empty());
    snapshot.now = jiff::Timestamp::from_second(1_240).unwrap();
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
            last_reclaimed_at: snapshot.now,
            wake_error: None,
            reclaimed_bytes: 300,
        },
    )
    .unwrap();
    std::sync::Arc::make_mut(&mut snapshot.agents)[0].status = crate::agents::AgentStatus::Sleeping;
    snapshot.now = jiff::Timestamp::from_second(3039).unwrap();
    assert!(spawned(&snapshot).is_empty());
    snapshot.now = jiff::Timestamp::from_second(3040).unwrap();
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
            last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
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
    let (_dir, store, agent, _policy, scope) = decision_fixture();
    let mut config = MachineConfig::default();
    config.agents.hibernate.enabled = Some(true);
    let pane: PaneId = "tmux:%3".parse().unwrap();
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
            last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
            wake_error: None,
            reclaimed_bytes: 0,
        },
    )
    .unwrap();
    produce(&snapshot);
    produce(&snapshot);
    snapshot.now = jiff::Timestamp::from_second(1_185).unwrap();
    produce(&snapshot);
    config.agents.hibernate.enabled = Some(false);
    snapshot.now = jiff::Timestamp::from_second(1_190).unwrap();
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
        [pane.clone(), pane.clone(), pane],
        "pulled focus must repair a missing event or a record published after focus, with pacing"
    );
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
            last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
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
            "run loginctl enable-linger <user> or log in through a systemd session",
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
        admit_with(&config, &host)
            .unwrap()
            .reason
            .as_deref(),
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
