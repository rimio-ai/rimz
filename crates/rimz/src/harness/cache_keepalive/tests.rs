use super::*;
use crate::agents::{AgentStatus, PendingWait, PendingWaitTrigger};
use crate::config::{Profile, RoleBinding, Team};

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

fn sleeping() -> AgentState {
    let mut agent = AgentState::stub("claude", "session", AgentStatus::Idle);
    agent.turn_started_at = Some(ts(0));
    agent.turn_ended_at = Some(ts(10));
    agent.pending_waits.push(PendingWait {
        name: "gate".into(),
        trigger: PendingWaitTrigger::Command {
            command: "cargo xtask gate".into(),
        },
        armed_at: Some(ts(0)),
    });
    agent
}

#[test]
fn keepalive_window_and_recurrence_follow_requests_not_wait_arm_time() {
    let mut agent = sleeping();
    let config = HarnessConfig::default();
    assert!(should_keepalive(&agent, &config, None, ts(3540)));
    assert!(!should_keepalive(&agent, &config, None, ts(3539)));
    assert!(!should_keepalive(&agent, &config, None, ts(3600)));
    agent.turn_started_at = Some(ts(3545));
    agent.turn_ended_at = Some(ts(3550));
    assert!(!should_keepalive(&agent, &config, None, ts(3550)));
    assert_eq!(agent.keepalive_since, None);
    assert!(should_keepalive(&agent, &config, None, ts(7085)));
    agent.keepalive_since = Some(ts(0));
    assert!(
        should_keepalive(&agent, &config, None, ts(7085)),
        "under the cap"
    );
}

/// A run of pings since the real request at `since`, the last one at `ping`.
fn pinged_at(since: i64, ping: i64) -> AgentState {
    let mut agent = sleeping();
    agent.keepalive_since = Some(ts(since));
    agent.pinged_at = Some(ts(ping));
    agent
}

#[test]
fn keepalive_cap_refuses_a_ping_that_would_start_at_the_maximum() {
    let config = HarnessConfig::default();
    let cap = 6 * 3600;
    let allowed = pinged_at(0, cap - 3540 - 1);
    assert!(should_keepalive(&allowed, &config, None, ts(cap - 1)));
    let capped = pinged_at(0, cap - 3540);
    assert!(!should_keepalive(&capped, &config, None, ts(cap)));
    assert!(
        !should_keepalive(&pinged_at(100, 50), &config, None, ts(3590)),
        "a negative span is refused"
    );
    let mut uncapped = config.clone();
    uncapped.cache_keepalive_max = None;
    assert!(should_keepalive(&capped, &uncapped, None, ts(cap)));
    let mut first = capped;
    first.keepalive_since = None;
    assert!(
        should_keepalive(&first, &config, None, ts(cap)),
        "the first ping of a sleep is never capped"
    );
}

#[test]
fn final_ping_is_the_one_whose_successor_would_reach_the_cap() {
    let config = HarnessConfig::default();
    let max = Some(Duration::from_secs(6 * 3600));
    let cap = 6 * 3600;
    let fresh = sleeping();
    assert_eq!(final_ping(&fresh, &config, ts(cap - 3540 - 1)), None);
    assert_eq!(
        final_ping(&fresh, &config, ts(cap - 3540)),
        max,
        "falls back to the last request"
    );
    let run = pinged_at(100, 3640);
    assert_eq!(final_ping(&run, &config, ts(cap - 3540)), None);
    assert_eq!(final_ping(&run, &config, ts(cap - 3440)), max);
    let mut uncapped = config.clone();
    uncapped.cache_keepalive_max = None;
    assert_eq!(final_ping(&run, &uncapped, ts(cap)), None);
    let after_final = pinged_at(100, cap - 3440);
    assert!(
        !should_keepalive(&after_final, &config, None, ts(cap + 100)),
        "no ping follows a final one"
    );
}

#[test]
fn keepalive_excludes_non_sleepers_and_disabled_or_unsafe_seats() {
    let agent = sleeping();
    let config = HarnessConfig::default();
    assert!(
        should_keepalive(&agent, &config, None, ts(3540)),
        "solo agent qualifies"
    );
    let mut changed = agent.clone();
    changed.pending_waits.clear();
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.status = AgentStatus::Running;
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.compacting_since = Some(ts(3539));
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.turn_ended_at = None;
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.parent_agent_id = Some("parent".into());
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.status = AgentStatus::Waiting;
    changed.waiting_since = Some(changed.last_activity);
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.kind = AgentKind::new_unchecked("amp");
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 1.0,
        spend_usd: 1.0,
        window: crate::agents::BudgetWindow::Session,
        at: ts(0),
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
    });
    assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    changed = agent.clone();
    changed.pending_waits[0].trigger = PendingWaitTrigger::Timer {
        due: ts(7200),
        delay: Some("2h".into()),
    };
    assert!(should_keepalive(&changed, &config, None, ts(3540)));
    for trigger in [
        PendingWaitTrigger::Subagent {
            active_at: ts(3540),
            deadline_at: None,
            settled: None,
        },
        PendingWaitTrigger::Team {
            stage: Some("Review".into()),
        },
    ] {
        changed = agent.clone();
        changed.pending_waits[0].trigger = trigger;
        assert!(should_keepalive(&changed, &config, None, ts(3540)));
        changed.pending_waits.clear();
        assert!(!should_keepalive(&changed, &config, None, ts(3540)));
    }
    let mut disabled = config;
    disabled.cache_keepalive = false;
    assert!(!should_keepalive(&agent, &disabled, None, ts(3540)));
}

#[test]
fn keepalive_prompt_is_neutral_and_includes_each_wait() {
    let mut agent = sleeping();
    agent.pending_waits.insert(
        0,
        PendingWait {
            name: "forge#feat-x".into(),
            trigger: serde_json::from_value(serde_json::json!({"kind": "team", "stage": "Review"}))
                .unwrap(),
            armed_at: Some(ts(-7340)),
        },
    );
    for (name, settled) in [("bright-owl", Some("completed")), ("calm-fox", None)] {
        agent.pending_waits.insert(
            0,
            PendingWait {
                name: name.into(),
                trigger: serde_json::from_value(serde_json::json!({
                    "kind": "subagent", "active_at": ts(3280),
                    "deadline_at": ts(4600), "settled": settled,
                }))
                .unwrap(),
                armed_at: Some(ts(960)),
            },
        );
    }
    agent.pending_waits.push(PendingWait {
        name: "ci".into(),
        trigger: PendingWaitTrigger::Signal {
            selector: "pr.checks".into(),
        },
        armed_at: None,
    });
    agent.pending_waits.push(PendingWait {
        name: "nap".into(),
        trigger: PendingWaitTrigger::Timer {
            due: ts(4240),
            delay: None,
        },
        armed_at: Some(ts(3180)),
    });
    let text = "Cache keepalive, no action needed. Waiting on:\n- calm-fox: 42m, active 3m ago, deadline in 18m\n- bright-owl: 42m, completed, reporting\n- forge#feat-x: 3h, stage Review\n- gate: cargo xtask gate, 58m\n- ci: signal pr.checks\n- nap: timer in 12m, 5m";
    assert_eq!(prompt(&agent, ts(3485), None), text);
    assert_eq!(
        prompt(&agent, ts(3485), Some(Duration::from_secs(90 * 60))),
        format!("{text}\nKeepalive limit 90m reached: this is the last ping until your next turn.")
    );
}

#[test]
fn keepalive_producers_share_pacing_even_during_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let agent = sleeping();
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![agent.clone()], ts(3540));
    let config = HarnessConfig::default();
    let policy = KeepWarmPolicy::default();
    keepalive_agents_with(&snapshot, &runtime, &config, &policy, |_| {
        panic!("no live pane")
    });
    snapshot
        .agent_panes
        .push(crate::store::snapshot::PaneAgent {
            root_lane: false,
            kind: agent.kind.clone(),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::parse("tmux:%1").unwrap(),
            pane_pid: None,
            worktree_path: None,
            worktree_branch: None,
        });
    let mut count = 0;
    keepalive_agents_with(&snapshot, &runtime, &config, &policy, |request| {
        assert_eq!(request.anchor, ts(0));
        assert!(
            request
                .target(&snapshot, &config, &policy, ts(3540))
                .is_some()
        );
        let mut changed = snapshot.clone();
        changed.agents[0].last_tool_at = Some(ts(1));
        assert!(
            request
                .target(&changed, &config, &policy, ts(3541))
                .is_none(),
            "moved anchor"
        );
        changed = snapshot.clone();
        changed.agents[0].pending_waits.clear();
        assert!(
            request
                .target(&changed, &config, &policy, ts(3540))
                .is_none(),
            "wait completed"
        );
        changed = snapshot.clone();
        changed.agent_panes.clear();
        assert!(
            request
                .target(&changed, &config, &policy, ts(3540))
                .is_none(),
            "pane vanished"
        );
        count += 1;
        keepalive_agents_with(&snapshot, &runtime, &config, &policy, |_| {
            panic!("concurrent spawn")
        });
    });
    assert_eq!(count, 1);
    keepalive_agents_with(&snapshot, &runtime, &config, &policy, |_| {
        panic!("repeated anchor")
    });
    let stale = snapshot.clone();
    snapshot.agents[0].turn_started_at = Some(ts(3545));
    snapshot.agents[0].turn_ended_at = Some(ts(3550));
    snapshot.now = ts(7085);
    keepalive_agents_with(&snapshot, &runtime, &config, &policy, |_| {
        count += 1;
    });
    assert_eq!(count, 2);
    keepalive_agents_with(&stale, &runtime, &config, &policy, |_| {
        panic!("stale producer respawned an older anchor")
    });
}

const TWO_HOURS: Duration = Duration::from_secs(7200);

fn idle() -> AgentState {
    let mut agent = sleeping();
    agent.pending_waits.clear();
    agent
}

fn profile(keep_warm: Option<KeepWarm>) -> Profile {
    let mut profile: Profile = toml::from_str("agent = \"claude\"").unwrap();
    profile.keep_warm = keep_warm;
    profile
}

fn team(keep_warm: Option<KeepWarm>) -> Team {
    let binding = |role: &str| RoleBinding {
        keep_warm: (role == "coder").then_some(keep_warm).flatten(),
        ..toml::from_str(&format!("role = \"{role}\"\nprofile = \"warm\"")).unwrap()
    };
    let mut team: Team = toml::from_str("").unwrap();
    team.roles = vec![binding("coder"), binding("judge")];
    team
}

/// A solo profile `warm` and a team `forge` whose `coder` role holds for `role`.
fn policy(solo: Option<KeepWarm>, role: Option<KeepWarm>) -> KeepWarmPolicy {
    let mut policy = KeepWarmPolicy::default();
    policy.profiles.0.insert("warm".into(), profile(solo));
    policy.teams.0.insert("forge".into(), team(role));
    policy
}

#[test]
fn keep_warm_horizon_follows_role_or_profile_and_ttl_admission() {
    let two_hours = Some(KeepWarm::For(TWO_HOURS));
    let config = HarnessConfig::default();
    let mut solo = idle();
    solo.profile = Some("warm".into());
    assert_eq!(
        policy(two_hours, None).horizon(&solo, &config),
        Some(TWO_HOURS)
    );
    assert_eq!(policy(None, two_hours).horizon(&solo, &config), None);
    assert_eq!(
        policy(Some(KeepWarm::Off), None).horizon(&solo, &config),
        None
    );
    solo.profile = Some("other".into());
    assert_eq!(policy(two_hours, None).horizon(&solo, &config), None);

    let mut seat = idle();
    seat.profile = Some("warm".into());
    seat.team = Some("forge".into());
    seat.role = Some("coder".into());
    assert_eq!(
        policy(None, two_hours).horizon(&seat, &config),
        Some(TWO_HOURS)
    );
    assert_eq!(
        policy(two_hours, None).horizon(&seat, &config),
        None,
        "a seat's role decides, not the profile behind it"
    );
    seat.role = Some("judge".into());
    assert_eq!(policy(two_hours, two_hours).horizon(&seat, &config), None);

    let held = policy(two_hours, None);
    solo.profile = Some("warm".into());
    for (kind, harness, expected) in [
        ("amp", "", None),
        ("codex", "", Some(TWO_HOURS)),
        ("amp", "[prompt_cache_ttl]\namp = \"20m\"", Some(TWO_HOURS)),
        ("claude", "[prompt_cache_ttl]\nclaude = \"5m\"", None),
        (
            "claude",
            "keep_warm_min_ttl = \"off\"\n[prompt_cache_ttl]\nclaude = \"5m\"",
            Some(TWO_HOURS),
        ),
        ("codex", "keep_warm_min_ttl = \"45m\"", None),
        ("claude", "[prompt_cache_ttl]\nclaude = \"off\"", None),
    ] {
        solo.kind = AgentKind::new_unchecked(kind);
        let harness: HarnessConfig = toml::from_str(harness).unwrap();
        assert_eq!(
            held.horizon(&solo, &harness),
            expected,
            "{kind} {harness:?}"
        );
    }
}

#[test]
fn keep_warm_follows_a_toml_base_chain_and_skips_launched_children() {
    let config = HarnessConfig::default();
    let mut held = policy(Some(KeepWarm::For(TWO_HOURS)), None);
    let child = |keep_warm| {
        let mut profile: Profile = toml::from_str("agent = \"warm\"").unwrap();
        profile.keep_warm = keep_warm;
        profile
    };
    held.profiles.0.insert("child".into(), child(None));
    held.profiles.0.insert("grandchild".into(), {
        let mut profile: Profile = toml::from_str("agent = \"child\"").unwrap();
        profile.keep_warm = None;
        profile
    });
    held.profiles
        .0
        .insert("cold".into(), child(Some(KeepWarm::Off)));
    let mut agent = idle();
    for (profile, expected) in [
        ("child", Some(TWO_HOURS)),
        ("grandchild", Some(TWO_HOURS)),
        ("cold", None),
    ] {
        agent.profile = Some(profile.into());
        assert_eq!(held.horizon(&agent, &config), expected, "{profile}");
    }
    agent.profile = Some("warm".into());
    agent.parent_agent_id = Some("parent".into());
    agent.launch_depth = Some(1);
    assert_eq!(
        held.horizon(&agent, &config),
        None,
        "a rimz subagents child never resolves a horizon"
    );
}

#[test]
fn keep_warm_pings_an_idle_agent_each_window_until_the_horizon() {
    let config = HarnessConfig::default();
    let mut agent = idle();
    agent.profile = Some("warm".into());
    let held = policy(Some(KeepWarm::For(TWO_HOURS)), None);
    let due = |agent: &AgentState, now: i64| {
        should_keepalive(
            agent,
            &config,
            held.holding(agent, &config, ts(now)),
            ts(now),
        )
    };
    assert!(
        !should_keepalive(&agent, &config, None, ts(3540)),
        "no keep-warm"
    );
    assert!(!due(&agent, 3539));
    assert!(due(&agent, 3540));
    assert!(!due(&agent, 3600));
    agent.status = AgentStatus::Success;
    assert!(due(&agent, 3540));
    for status in [
        AgentStatus::Running,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ] {
        agent.status = status;
        assert!(!due(&agent, 3540), "{status:?}");
    }
    agent.status = AgentStatus::Idle;
    // A ping moves only the request anchor, never the last real turn's end.
    agent.pinged_at = Some(ts(3545));
    assert!(due(&agent, 7085));
    agent.pinged_at = Some(ts(7090));
    assert!(!due(&agent, 10_630), "horizon passed at 7210");
    assert!(
        policy(Some(KeepWarm::For(Duration::from_secs(3 * 3600))), None)
            .holding(&agent, &config, ts(10_630))
            .is_some()
    );
    agent.turn_started_at = Some(ts(7092));
    agent.turn_ended_at = Some(ts(7095));
    assert!(due(&agent, 10_632), "a real turn restarts the horizon");

    let mut disabled = config.clone();
    disabled.cache_keepalive = false;
    assert!(!should_keepalive(
        &agent,
        &disabled,
        held.holding(&agent, &disabled, ts(10_632)),
        ts(10_632)
    ));
}

#[test]
fn keep_warm_stops_for_a_finished_cohort_but_the_sleeping_path_does_not() {
    let config = HarnessConfig::default();
    let root = tempfile::tempdir().unwrap();
    let mut seat = idle();
    seat.team = Some("forge".into());
    seat.role = Some("coder".into());
    seat.worktree_path = Some(root.path().to_string_lossy().into_owned());
    let held = policy(None, Some(KeepWarm::For(TWO_HOURS)));
    let due = |agent: &AgentState| {
        should_keepalive(
            agent,
            &config,
            held.holding(agent, &config, ts(3540)),
            ts(3540),
        )
    };
    std::fs::write(
        root.path().join("blackboard.md"),
        "Stage: Review (@judge)\n",
    )
    .unwrap();
    assert!(due(&seat));
    std::fs::write(root.path().join("blackboard.md"), "Stage: Done\n").unwrap();
    assert!(!due(&seat), "Done ends the hold");
    let mut sleeper = sleeping();
    sleeper.team = seat.team.clone();
    sleeper.role = seat.role.clone();
    sleeper.worktree_path = seat.worktree_path.clone();
    assert!(due(&sleeper), "a pending wait still keeps its cache");
}

#[test]
fn keep_warm_holds_only_an_agent_it_can_still_ping() {
    let root = tempfile::tempdir().unwrap();
    let held = policy(
        Some(KeepWarm::For(TWO_HOURS)),
        Some(KeepWarm::For(TWO_HOURS)),
    );
    let on = HarnessConfig::default();
    let holding =
        |agent: &AgentState, config: &HarnessConfig| held.holding(agent, config, ts(3540));
    let mut solo = idle();
    solo.profile = Some("warm".into());
    assert_eq!(holding(&solo, &on), Some(TWO_HOURS));
    let mut success = solo.clone();
    success.status = AgentStatus::Success;
    assert_eq!(holding(&success, &on), Some(TWO_HOURS));
    // Sleeping is a rested row with armed waits, never a stored status.
    let mut asleep = sleeping();
    asleep.profile = Some("warm".into());
    assert_eq!(asleep.effective_status(), AgentStatus::Sleeping);
    assert_eq!(holding(&asleep, &on), Some(TWO_HOURS));

    let off: HarnessConfig = toml::from_str("cache_keepalive = false").unwrap();
    assert_eq!(holding(&solo, &off), None, "no ping can maintain the hold");
    let mut running = solo.clone();
    running.status = AgentStatus::Running;
    assert_eq!(holding(&running, &on), None);
    // A clean end parked on background work rests in the lifecycle itself, so
    // its pings fold inert and the hold stands. A Running row only a provider
    // marker settled rests where the durable fold cannot see, so a ping there
    // would fold as work: it is not held.
    let mut background = running.clone();
    background.phase = crate::agents::TurnPhase::Parked;
    assert_eq!(background.effective_status(), AgentStatus::Success);
    assert_eq!(holding(&background, &on), Some(TWO_HOURS));
    let mut settled = running.clone();
    settled.last_activity = ts(10);
    settled.context = Some(crate::agents::AgentContext {
        settle: Some(crate::agents::TurnSettle::new(
            ts(20),
            crate::agents::TurnSettleOutcome::Complete,
        )),
        ..crate::agents::AgentContext::default()
    });
    assert_eq!(settled.effective_status(), AgentStatus::Success);
    assert_eq!(holding(&settled, &on), None);
    let mut compacting = solo.clone();
    compacting.compacting_since = Some(ts(3000));
    assert_eq!(holding(&compacting, &on), None);
    let mut parked = solo.clone();
    parked.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 1.0,
        spend_usd: 1.0,
        window: crate::agents::BudgetWindow::Session,
        at: Timestamp::UNIX_EPOCH,
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
    });
    assert_eq!(holding(&parked, &on), None);
    let mut asking = solo.clone();
    asking.status = AgentStatus::Waiting;
    asking.waiting_since = Some(asking.last_activity);
    assert_eq!(holding(&asking, &on), None);
    let mut child = solo.clone();
    child.parent_agent_id = Some("parent".into());
    assert_eq!(holding(&child, &on), None);

    let mut seat = solo.clone();
    seat.team = Some("forge".into());
    seat.role = Some("coder".into());
    seat.worktree_path = Some(root.path().to_string_lossy().into_owned());
    assert_eq!(holding(&seat, &on), Some(TWO_HOURS));
    std::fs::write(root.path().join("blackboard.md"), "Stage: Done\n").unwrap();
    assert_eq!(holding(&seat, &on), None, "a Done cohort is not held");

    assert_eq!(
        holding(&solo, &on).map(|_| held.holding(&solo, &on, ts(3600))),
        Some(None),
        "a missed window leaves the cache cold, so nothing holds it"
    );
    let mut stopping = solo.clone();
    stopping.idle_stop = Some(crate::agents::PendingIdleStop {
        stop: crate::agents::IdleStop {
            after_secs: 600,
            requested_at: ts(20),
            requested_by: None,
        },
        due_at: None,
    });
    assert_eq!(
        holding(&stopping, &on),
        None,
        "a requested stop ends the hold"
    );

    let short: HarnessConfig = toml::from_str("[prompt_cache_ttl]\nclaude = \"5m\"").unwrap();
    assert_eq!(
        holds(&solo, Some(TWO_HOURS), &short, ts(3540)),
        None,
        "a published horizon is re-admitted against the current floor"
    );
    assert_eq!(
        holds(&solo, Some(TWO_HOURS), &on, ts(3540)),
        Some(TWO_HOURS)
    );
}

#[test]
fn keepalive_prompt_names_waits_only_when_there_are_some() {
    assert_eq!(
        prompt(&idle(), ts(3540), None),
        "Cache keepalive, no action needed."
    );
}

#[test]
fn keep_warm_holds_only_while_the_cap_permits_another_ping() {
    let config = HarnessConfig::default();
    let cap = 6 * 3600;
    let held = policy(Some(KeepWarm::For(Duration::from_secs(12 * 3600))), None);
    let mut agent = idle();
    agent.profile = Some("warm".into());
    agent.keepalive_since = Some(ts(0));
    let due = |agent: &AgentState, now: i64| {
        let holding = held.holding(agent, &config, ts(now));
        (holding, should_keepalive(agent, &config, holding, ts(now)))
    };
    agent.pinged_at = Some(ts(cap - 3541));
    assert_eq!(
        due(&agent, cap - 1),
        (Some(Duration::from_secs(12 * 3600)), true),
        "the final ping is still held"
    );
    agent.pinged_at = Some(ts(cap - 3540));
    assert_eq!(
        due(&agent, cap - 3000),
        (None, false),
        "after the final ping the horizon no longer holds"
    );
    let mut uncapped = config.clone();
    uncapped.cache_keepalive_max = None;
    assert!(held.holding(&agent, &uncapped, ts(cap - 3000)).is_some());
}

#[test]
fn helper_rechecks_the_horizon_before_pinging() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut agent = idle();
    agent.profile = Some("warm".into());
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![agent.clone()], ts(3540));
    snapshot
        .agent_panes
        .push(crate::store::snapshot::PaneAgent {
            root_lane: false,
            kind: agent.kind.clone(),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::parse("tmux:%1").unwrap(),
            pane_pid: None,
            worktree_path: None,
            worktree_branch: None,
        });
    let config = HarnessConfig::default();
    let held = policy(Some(KeepWarm::For(TWO_HOURS)), None);
    keepalive_agents_with(
        &snapshot,
        &runtime,
        &config,
        &KeepWarmPolicy::default(),
        |_| panic!("no keep-warm, no ping"),
    );
    let mut requests = Vec::new();
    keepalive_agents_with(&snapshot, &runtime, &config, &held, |request| {
        requests.push(request.clone());
    });
    assert_eq!(requests.len(), 1);
    let (_, holding) = requests[0]
        .target(&snapshot, &config, &held, ts(3540))
        .expect("still due");
    assert_eq!(holding, Some(TWO_HOURS));
    assert!(
        requests[0]
            .target(&snapshot, &config, &KeepWarmPolicy::default(), ts(3540))
            .is_none(),
        "keep-warm removed from the definition since the producer fired"
    );
    let mut shortened = snapshot.clone();
    shortened.agents[0].turn_ended_at = Some(ts(-7200));
    assert!(
        requests[0]
            .target(&shortened, &config, &held, ts(3540))
            .is_none(),
        "horizon passed since the producer fired"
    );
}
