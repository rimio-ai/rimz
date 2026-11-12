use super::*;
use std::collections::BTreeMap;

use crate::agents::credits::CreditsCache;
use crate::agents::{
    AgentStatus, Entitlement, ExtraCredits, PendingWait, PendingWaitTrigger, ProviderCapacity,
    RateLimitWindow,
};
use crate::ids::{MuxName, WorkspaceId};
use crate::store::snapshot::PaneAgent;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("timestamp")
}

fn agent(status: AgentStatus, activity: i64, tokens: u64) -> AgentState {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("session-1"),
        status,
        ts(activity),
    );
    agent.team = Some("probe".to_owned());
    agent.turn_started_at = Some(ts(activity));
    agent.turn_ended_at = Some(ts(activity));
    agent.usage.total_tokens = Some(tokens);
    agent.worktree_path = Some("/repo/worktree".to_owned());
    agent.worktree_branch = Some("feat/cache".to_owned());
    agent
}

#[test]
fn fire_cache_path_preserves_existing_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");

    assert_eq!(
        fire_record_path(
            &runtime,
            &AgentKind::new_unchecked("claude"),
            &"sess".into()
        )
        .file_name()
        .and_then(|name| name.to_str()),
        Some("4a8d94f232e55a6a0879ba0858b59241.json")
    );
}

/// Default harness: Claude's 60m TTL less the 1m margin opens the window at 59m.
const NOW: i64 = 6_000 + 3_550;

fn due(agent: &AgentState) -> bool {
    should_compact(
        agent,
        Some("/compact"),
        fire_point(agent, Some(50_000), &HarnessConfig::default()),
        ts(NOW),
    )
}

#[test]
fn predicate_requires_idle_threshold_and_the_seat_floor() {
    let candidate = agent(AgentStatus::Idle, 6_000, 50_000);
    assert!(due(&candidate));
    assert!(!due(&agent(AgentStatus::Idle, 6_011, 50_000)));
    assert!(
        !due(&agent(AgentStatus::Idle, 6_000, 49_999)),
        "one below the floor"
    );
    let mut compacted = candidate.clone();
    compacted.compacted_awaiting_prompt = Some(ts(6_000));
    assert!(!due(&compacted), "a compacted seat waits for a real prompt");
    assert!(!should_compact(
        &candidate,
        None,
        fire_point(&candidate, Some(50_000), &HarnessConfig::default()),
        ts(NOW),
    ));
    assert_eq!(
        fire_point(&candidate, None, &HarnessConfig::default()),
        None,
        "an off floor opens no window"
    );
}

#[test]
fn window_opens_at_ttl_less_the_configured_margin_and_closes_at_ttl() {
    let candidate = agent(AgentStatus::Idle, 0, 80_000);
    let harness: HarnessConfig = toml::from_str("prompt_cache_margin = \"6m\"").unwrap();
    let window = fire_point(&candidate, Some(50_000), &harness);
    assert_eq!(
        window.map(|window| window.fire_after.as_secs()),
        Some(54 * 60)
    );
    for (now, expected) in [
        (54 * 60 - 1, false),
        (54 * 60, true),
        (60 * 60 - 1, true),
        (60 * 60, false),
        (86_400, false),
    ] {
        assert_eq!(
            should_compact(&candidate, Some("/compact"), window, ts(now)),
            expected,
            "{now}"
        );
    }
}

#[test]
fn observed_short_lifetime_compacts_without_a_ping_floor_but_above_margin() {
    let mut candidate = agent(AgentStatus::Idle, 0, 80_000);
    candidate.context = Some(crate::agents::AgentContext {
        last_cache_write: Some(crate::agents::context::CacheWriteRetention::FiveMinutes),
        ..Default::default()
    });
    let actual: Vec<_> = [60, 300, 360]
        .map(|margin| {
            let mut harness = HarnessConfig::default();
            harness.prompt_cache_margin = Duration::from_secs(margin);
            let window = fire_point(&candidate, Some(50_000), &harness);
            (
                window.map(|window| window.fire_after.as_secs()),
                should_compact(&candidate, Some("/compact"), window, ts(240)),
            )
        })
        .into_iter()
        .collect();
    assert_eq!(actual, [(Some(240), true), (None, false), (None, false)]);
}

#[test]
fn predicate_clamps_request_anchor_after_resume() {
    let mut candidate = agent(AgentStatus::Idle, 30 * 60, 80_000);
    candidate.turn_ended_at = Some(ts(0));
    let window = fire_point(&candidate, Some(50_000), &HarnessConfig::default());
    for (now, expected) in [(59 * 60 - 1, false), (59 * 60, true), (60 * 60, false)] {
        assert_eq!(
            should_compact(&candidate, Some("/compact"), window, ts(now)),
            expected
        );
    }
}

#[test]
fn predicate_requires_a_completed_request() {
    let mut candidate = agent(AgentStatus::Idle, 0, 80_000);
    candidate.turn_ended_at = None;
    assert!(!due(&candidate));
}

#[test]
fn predicate_counts_idle_from_the_last_request_not_the_response_end() {
    let mut candidate = agent(AgentStatus::Idle, 5_000, 80_000);
    candidate.last_tool_at = Some(ts(6_000));
    candidate.turn_ended_at = Some(ts(9_000));
    assert!(due(&candidate));
    candidate.last_tool_at = Some(ts(7_000));
    assert!(!due(&candidate));
}

#[test]
fn solo_seats_and_team_seats_compact_whatever_the_board_says() {
    let mut candidate = agent(AgentStatus::Idle, 6_000, 80_000);
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    candidate.worktree_path = Some(root.path().to_string_lossy().into_owned());
    candidate.team = None;
    assert!(due(&candidate), "a solo seat reads no board");
    candidate.team = Some("probe".to_owned());
    let root = tempfile::tempdir().unwrap();
    candidate.worktree_path = Some(root.path().to_string_lossy().into_owned());
    assert!(due(&candidate), "no board is live");
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Review (@judge)\n",
    )
    .unwrap();
    assert!(due(&candidate));
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    assert!(due(&candidate), "Done still compacts");
}

#[test]
fn cache_timing_reaches_the_request_before_expiry() {
    // Request anchoring leaves only tick, spawn, and submit latency.
    const PRODUCER_TICK_BOUND: u64 = 1;
    const HELPER_SPAWN_BOUND: u64 = 10;
    const KEYSTROKE_TO_REQUEST_BOUND: u64 = 10;
    for (kind, model, config, ttl) in [
        ("claude", "any", "", Some(3600)),
        ("codex", "gpt-5.6-terra", "", Some(1800)),
        ("codex", "gpt-6-luna", "", Some(1800)),
        ("codex", "gpt-5-codex", "", Some(1800)),
        ("amp", "any", "", Some(3600)),
        ("amp", "any", "[prompt_cache_ttl]\nfallback = 'off'", None),
        (
            "claude",
            "any",
            "[prompt_cache_ttl]\nclaude = \"5m\"",
            Some(300),
        ),
        (
            "claude",
            "any",
            "[prompt_cache_ttl]\nclaude = \"off\"",
            None,
        ),
    ] {
        let mut candidate = agent(AgentStatus::Idle, 0, 80_000);
        candidate.kind = AgentKind::new_unchecked(kind);
        candidate.model = Some(model.to_owned());
        let harness: HarnessConfig = toml::from_str(config).unwrap();
        let point = fire_point(&candidate, Some(50_000), &harness);
        assert_eq!(
            point.map(|point| point.fire_after.as_secs()),
            ttl.map(|ttl| ttl - 60),
            "{kind}/{model}"
        );
        if let (Some(point), Some(ttl)) = (point, ttl) {
            assert!(
                point.fire_after.as_secs()
                    + PRODUCER_TICK_BOUND
                    + HELPER_SPAWN_BOUND
                    + KEYSTROKE_TO_REQUEST_BOUND
                    < ttl
            );
            for (idle, expected) in [
                (point.fire_after.as_secs(), true),
                (point.fire_after.as_secs() - 1, false),
                (ttl - 1, true),
                (ttl, false),
            ] {
                assert_eq!(
                    should_compact(&candidate, Some("/compact"), Some(point), ts(idle as i64)),
                    expected
                );
            }
        }
    }
}

#[test]
fn producer_fires_at_each_seats_own_floor_with_its_brief() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let mut candidate = agent(AgentStatus::Idle, 6_100, 80_000);
    candidate.turn_ended_at = Some(ts(6_000));
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![candidate.clone()], ts(NOW));
    std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
        checkout: None,
        root_lane: false,
        lane: None,
        kind: candidate.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(candidate.agent_id.clone()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: candidate.worktree_path.clone(),
        worktree_branch: candidate.worktree_branch.clone(),
    });
    let config = crate::config::MachineConfig::default();
    let mut policy = SeatPolicy::default();
    for name in ["solo", "probe.lead"] {
        let mut profile: crate::config::Profile = toml::from_str("agent = \"claude\"").unwrap();
        profile.idle_compact = Some(crate::config::IdleCompact::Tokens(80_000));
        policy.profiles.0.insert(name.to_owned(), profile);
    }
    let mut team: crate::config::Team =
        toml::from_str("[[roles]]\nrole = \"coder\"\nprofile = \"claude\"").unwrap();
    team.roles[0].idle_compact = Some(crate::config::IdleCompact::Tokens(80_000));
    policy.teams.0.insert("probe".to_owned(), team);
    let path = fire_record_path(&runtime, &candidate.kind, &candidate.agent_id);
    let mut fire = |seat: &AgentState| {
        std::sync::Arc::make_mut(&mut snapshot.agents)[0] = seat.clone();
        let mut requests = Vec::new();
        compact_idle_agents_with(
            &snapshot,
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            |request| {
                requests.push(request.clone());
                true
            },
        );
        if !requests.is_empty() {
            assert_eq!(
                Some(read_fire_record(&path).unwrap().fired_for_request),
                seat.last_request_at()
            );
            std::fs::remove_file(&path).unwrap();
        }
        requests
    };

    assert!(
        fire(&candidate).is_empty(),
        "80k sits below the 100k default"
    );

    let mut toml_role = candidate.clone();
    toml_role.role = Some("coder".to_owned());
    toml_role.launch_depth = Some(1);
    let requests = fire(&toml_role);
    assert_eq!(
        requests.len(),
        1,
        "an agent-launched team peer's role override"
    );
    assert_eq!(
        requests[0].command,
        crate::agents::compact_command(&toml_role, &config.harness).unwrap()
    );
    assert!(requests[0].command.contains("This seat is a team member"));

    let mut frozen = toml_role.clone();
    frozen.hibernation = Some(crate::agents::state::Hibernation {
        frozen_at: ts(9_000),
        step: 0,
        reclaimed_bytes: 200,
        wake_error: None,
    });
    assert_eq!(fire(&frozen).len(), 1, "a hibernated seat still compacts");

    let mut markdown_role = candidate.clone();
    markdown_role.role = Some("lead".to_owned());
    markdown_role.profile = Some("probe.lead".to_owned());
    assert_eq!(
        fire(&markdown_role).len(),
        1,
        "a Markdown role's synthetic profile"
    );

    let mut solo = candidate.clone();
    solo.team = None;
    solo.profile = Some("solo".to_owned());
    let requests = fire(&solo);
    assert_eq!(requests.len(), 1, "a solo seat's profile floor");
    assert_eq!(
        requests[0].command,
        crate::agents::compact_command(&solo, &config.harness).unwrap()
    );
    assert!(!requests[0].command.contains("This seat is a team member"));

    solo.parent_agent_id = Some(AgentSessionId::from("parent"));
    solo.launch_depth = Some(1);
    assert!(
        fire(&solo).is_empty(),
        "a rimz subagents child never compacts"
    );
    assert!(!path.exists());
}

#[test]
fn frozen_seats_remain_eligible_for_idle_compaction() {
    let mut candidate = agent(AgentStatus::Idle, 6_000, 50_000);
    assert!(due(&candidate));
    candidate.hibernation = Some(crate::agents::state::Hibernation {
        frozen_at: ts(9_000),
        step: 0,
        reclaimed_bytes: 200,
        wake_error: None,
    });
    assert!(due(&candidate), "a frozen seat is still due");
}

#[test]
fn predicate_skips_busy_parked_compacting_and_child_agents() {
    let mut sleeping = agent(AgentStatus::Idle, 6_000, 50_000);
    sleeping.pending_waits.push(PendingWait {
        class: crate::agents::PendingWaitClass::Working,
        name: "wait-command".to_owned(),
        trigger: PendingWaitTrigger::Command {
            command: "cargo xtask gate".to_owned(),
        },
        armed_at: Some(ts(6_000)),
    });
    assert_eq!(sleeping.effective_status(ts(6_000)), AgentStatus::Sleeping);
    assert!(!due(&sleeping));

    for status in [
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ] {
        assert!(!due(&agent(status, 6_000, 50_000)), "{status:?}");
    }

    let mut parked = agent(AgentStatus::Idle, 6_000, 50_000);
    parked.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 1.0,
        spend_usd: 1.0,
        window: crate::agents::BudgetWindow::Session,
        at: ts(6_000),
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });
    assert!(!due(&parked));

    let mut compacting = agent(AgentStatus::Idle, 6_000, 50_000);
    compacting.compacting_since = Some(ts(9_000));
    assert!(!due(&compacting));

    let mut child = agent(AgentStatus::Idle, 6_000, 50_000);
    child.parent_agent_id = Some(AgentSessionId::from("parent"));
    assert!(!due(&child));
}

#[test]
fn predicate_deduplicates_context_and_paces_spawns() {
    let mut candidate = agent(AgentStatus::Idle, 6_000, 80_000);
    candidate.last_compact_command_tokens = Some(80_000);
    assert!(!due(&candidate));
    candidate.last_compact_command_tokens = None;

    let same_turn = FireRecord {
        fired_at: ts(8_000),
        fired_for_request: ts(6_000),
    };
    let mut resumed = agent(AgentStatus::Idle, 7_800, 80_000);
    resumed.turn_ended_at = candidate.turn_ended_at;
    assert!(!spawn_due(&resumed, ts(10_000), Some(&same_turn)));
    candidate.turn_ended_at = Some(ts(8_500));
    assert!(!spawn_due(&candidate, ts(10_000), Some(&same_turn)));
    // The provider's record of the same request is no new request.
    candidate.context = Some(crate::agents::AgentContext {
        recorded_request_at: Some(ts(6_001)),
        ..Default::default()
    });
    assert_eq!(candidate.last_request_at(), Some(ts(6_001)));
    assert!(!spawn_due(&candidate, ts(10_000), Some(&same_turn)));
    candidate.context = None;
    candidate.last_tool_at = Some(ts(8_000));
    assert!(spawn_due(&candidate, ts(10_000), Some(&same_turn)));

    let recent = FireRecord {
        fired_at: ts(9_500),
        fired_for_request: ts(5_000),
    };
    assert!(!spawn_due(&candidate, ts(10_000), Some(&recent)));
}

#[test]
fn keep_warm_holding_defers_compaction_until_the_horizon() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let mut seat = agent(AgentStatus::Idle, 0, 80_000);
    seat.role = Some("coder".to_owned());
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![seat.clone()], ts(3_540));
    std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
        checkout: None,
        root_lane: false,
        lane: None,
        kind: seat.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(seat.agent_id.clone()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: seat.worktree_path.clone(),
        worktree_branch: seat.worktree_branch.clone(),
    });
    let mut policy = SeatPolicy::default();
    let mut team: crate::config::Team =
        toml::from_str("[[roles]]\nrole = \"coder\"\nprofile = \"claude\"\nkeep-warm = \"2h\"")
            .unwrap();
    team.roles[0].idle_compact = Some(crate::config::IdleCompact::Tokens(50_000));
    policy.teams.0.insert("probe".to_owned(), team);
    let config = crate::config::MachineConfig::default();
    let mut spawned = 0;
    compact_idle_agents_with(
        &snapshot,
        &runtime,
        &RoomCapacities::default(),
        &config,
        &policy,
        |_| {
            spawned += 1;
            false
        },
    );
    assert_eq!(spawned, 0, "holding outranks idle compaction");
    compact_idle_agents_with(
        &snapshot,
        &runtime,
        &RoomCapacities::default(),
        &config,
        &SeatPolicy {
            teams: {
                let mut teams = policy.teams.clone();
                teams.0.get_mut("probe").unwrap().roles[0].keep_warm = None;
                teams
            },
            ..SeatPolicy::default()
        },
        |_| {
            spawned += 1;
            false
        },
    );
    assert_eq!(spawned, 1, "the same seat compacts without keep-warm");
    let mut pings_off = config.clone();
    pings_off.harness.cache_keepalive = false;
    compact_idle_agents_with(
        &snapshot,
        &runtime,
        &RoomCapacities::default(),
        &pings_off,
        &policy,
        |_| {
            spawned += 1;
            false
        },
    );
    assert_eq!(
        spawned, 2,
        "a horizon no ping maintains does not defer compaction"
    );
    // Past the horizon, the window runs from the last ping's request.
    std::sync::Arc::make_mut(&mut snapshot.agents)[0].pinged_at = Some(ts(7_100));
    snapshot.now = ts(7_100 + 3_540);
    compact_idle_agents_with(
        &snapshot,
        &runtime,
        &RoomCapacities::default(),
        &config,
        &policy,
        |_| {
            spawned += 1;
            false
        },
    );
    assert_eq!(spawned, 3, "compaction resumes once the horizon passed");
}

fn usage_window(used: u8, reset: i64, mins: u32, scope: Option<&str>) -> RateLimitWindow {
    RateLimitWindow {
        scope: scope.map(|id| crate::agents::RateLimitWindowScope {
            id: id.into(),
            label: id.into(),
        }),
        used_percentage: Some(used),
        resets_at: Some(ts(reset)),
        duration_mins: Some(mins),
        ..Default::default()
    }
}

/// The idle compactions the producer spawns for `seat` at `now`, and whether it paced one.
fn idle_fires(
    seat: &AgentState,
    now: i64,
    harness: &HarnessConfig,
    windows: Option<Vec<RateLimitWindow>>,
    credits: Option<CreditsCache>,
) -> (usize, bool) {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![seat.clone()], ts(now));
    std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
        checkout: None,
        root_lane: false,
        lane: None,
        kind: seat.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(seat.agent_id.clone()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: seat.worktree_path.clone(),
        worktree_branch: seat.worktree_branch.clone(),
    });
    let mut policy = SeatPolicy::default();
    let mut team: crate::config::Team =
        toml::from_str("[[roles]]\nrole = \"coder\"\nprofile = \"claude\"\nkeep-warm = \"2h\"")
            .unwrap();
    team.roles[0].idle_compact = Some(crate::config::IdleCompact::Tokens(50_000));
    policy.teams.0.insert("probe".to_owned(), team);
    let config = crate::config::MachineConfig {
        harness: harness.clone(),
        ..Default::default()
    };
    let capacities: RoomCapacities<'_> = windows
        .map(|windows| {
            BTreeMap::from([(seat.login_key(), ProviderCapacity::from_windows(windows))]).into()
        })
        .unwrap_or_default();
    let capacities = match credits {
        Some(credits) => capacities.with_credits(credits),
        None => capacities,
    };
    let mut fires = 0;
    compact_idle_agents_with(&snapshot, &runtime, &capacities, &config, &policy, |_| {
        fires += 1;
        true
    });
    let paced = fire_record_path(&runtime, &seat.kind, &seat.agent_id).exists();
    (fires, paced)
}

#[test]
fn a_spent_window_defers_idle_compaction_after_the_hold_lets_go() {
    let mut seat = agent(AgentStatus::Idle, 0, 80_000);
    seat.role = Some("coder".to_owned());
    seat.model = Some("claude-opus-4-6".to_owned());
    // Past the 2h horizon, with the last ping's cache still warm.
    let mut horizon = seat.clone();
    horizon.pinged_at = Some(ts(7_100));
    let horizon_now = 7_100 + 3_540;
    // Inside the horizon, with `cache_keepalive_max` reached.
    let mut capped = seat.clone();
    capped.keepalive_since = Some(ts(0));
    let capped_harness: HarnessConfig = toml::from_str("cache_keepalive_max = \"30m\"").unwrap();
    // A rejected ping left open on the resting row.
    let mut rejected = horizon.clone();
    rejected.ping_turn = true;
    let mut frozen = horizon.clone();
    frozen.hibernation = Some(crate::agents::state::Hibernation {
        frozen_at: ts(9_000),
        step: 0,
        reclaimed_bytes: 200,
        wake_error: None,
    });
    let spent = || {
        Some(vec![
            usage_window(40, 90_000, 300, None),
            usage_window(100, 90_000, 10_080, None),
        ])
    };
    let default = HarnessConfig::default();
    for (case, seat, now, harness) in [
        ("horizon passed", &horizon, horizon_now, &default),
        ("cap reached", &capped, 3_540, &capped_harness),
        ("rejected ping open", &rejected, horizon_now, &default),
        ("frozen seat", &frozen, horizon_now, &default),
    ] {
        assert_eq!(
            idle_fires(seat, now, harness, None, None),
            (1, true),
            "{case}: due with no reading"
        );
        assert_eq!(
            idle_fires(seat, now, harness, spent(), None),
            (0, false),
            "{case}: a spent window spawns nothing and paces nothing, so it fires after the reset"
        );
    }
    for (case, windows) in [
        ("reset passed", vec![usage_window(100, 9_000, 300, None)]),
        (
            "another model's sub-cap",
            vec![
                usage_window(40, 90_000, 300, None),
                usage_window(100, 90_000, 10_080, Some("model:sonnet")),
            ],
        ),
    ] {
        assert_eq!(
            idle_fires(&horizon, horizon_now, &default, Some(windows), None),
            (1, true),
            "{case}"
        );
    }
    assert_eq!(
        idle_fires(
            &horizon,
            horizon_now,
            &default,
            Some(vec![
                usage_window(40, 90_000, 300, None),
                usage_window(100, 90_000, 10_080, Some("model:opus")),
            ]),
            Some(paid(
                &horizon,
                ExtraCredits::Unlimited,
                Entitlement::Ok,
                horizon_now
            )),
        ),
        (0, false),
        "the model's own sub-cap, even with usable paid extra usage"
    );
    let disabled = ExtraCredits::Disabled;
    let exhausted = ExtraCredits::Known {
        used_usd: Some(50.0),
        remaining_usd: Some(0.0),
        limit_usd: Some(50.0),
    };
    let lapsed = Entitlement::Lapsed { since_ms: 1 };
    for (case, credits, fired) in [
        (
            "usable paid extra usage carries the window",
            Some(paid(
                &horizon,
                ExtraCredits::Unlimited,
                Entitlement::Ok,
                horizon_now,
            )),
            (1, true),
        ),
        (
            "paid extra usage disabled",
            Some(paid(&horizon, disabled, Entitlement::Ok, horizon_now)),
            (0, false),
        ),
        (
            "paid extra usage exhausted",
            Some(paid(&horizon, exhausted, Entitlement::Ok, horizon_now)),
            (0, false),
        ),
        (
            "a lapsed entitlement",
            Some(paid(&horizon, ExtraCredits::Unlimited, lapsed, horizon_now)),
            (0, false),
        ),
        (
            "no credits entry",
            Some(CreditsCache::default()),
            (0, false),
        ),
    ] {
        assert_eq!(
            idle_fires(&horizon, horizon_now, &default, spent(), credits),
            fired,
            "{case}"
        );
    }
}

/// A credits cache holding `seat`'s login with `extra` paid usage, observed at `now`.
fn paid(
    seat: &AgentState,
    extra: ExtraCredits,
    entitlement: Entitlement,
    now: i64,
) -> CreditsCache {
    CreditsCache::with_extra_credits(seat.login_key(), extra, entitlement, ts(now))
}

#[test]
fn the_helpers_recheck_reads_the_window_spent_since_the_producer() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let mut seat = agent(AgentStatus::Idle, 6_000, 80_000);
    seat.hibernation = Some(crate::agents::state::Hibernation {
        frozen_at: ts(9_000),
        step: 0,
        reclaimed_bytes: 200,
        wake_error: None,
    });
    let mut harness = HarnessConfig::default();
    harness.idle_compact = crate::config::IdleCompact::Tokens(50_000);
    let policy = SeatPolicy::default();
    let due = || {
        due_compaction(&seat, &policy, &harness, ts(NOW), || {
            ProviderCapacity::login_compaction_blocked(&runtime, &seat, ts(NOW))
        })
        .is_some()
    };
    assert!(due(), "no capacity cache compacts as today");
    let cache = crate::agents::RateLimitsCache {
        entries: [(
            seat.login_key().into(),
            crate::agents::RateLimitCacheEntry {
                limits: crate::agents::AgentRateLimits {
                    windows: vec![usage_window(100, 90_000, 300, None)],
                },
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    crate::disk::atomic::write_temp_then_rename_cache(
        &crate::agents::RateLimitsCache::shared_path(&runtime),
        &cache,
    )
    .unwrap();
    assert!(
        !due(),
        "a frozen seat at a spent window is not due, so the helper never wakes it"
    );
    let credits_path = runtime.shared_credits_path();
    std::fs::write(&credits_path, b"{not json").unwrap();
    assert!(!due(), "an unreadable credits cache carries nothing");
    crate::disk::atomic::write_temp_then_rename_cache(
        &credits_path,
        &paid(&seat, ExtraCredits::Unlimited, Entitlement::Ok, NOW),
    )
    .unwrap();
    assert!(due(), "usable paid extra usage carries the spent window");
}
