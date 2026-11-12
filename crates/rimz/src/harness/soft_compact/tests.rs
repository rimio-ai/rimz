use super::*;
use crate::agents::credits::CreditsCache;
use crate::agents::state::{SoftCompactMark, SoftCompactProgress};
use crate::agents::{AgentStatus, Entitlement, ExtraCredits};
use crate::ids::MuxName;
use crate::store::snapshot::PaneAgent;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

fn agent() -> AgentState {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "session".into(),
        AgentStatus::Running,
        ts(10),
    );
    agent.turn_started_at = Some(ts(10));
    agent.usage.total_tokens = Some(120_000);
    agent
}

fn snapshot(agent: &AgentState) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![agent.clone()],
        ts(20),
    );
    std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
        checkout: None,
        root_lane: false,
        lane: None,
        kind: agent.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(agent.agent_id.clone()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    });
    snapshot
}

fn due(agent: &AgentState) -> Option<SoftCompactStep> {
    due_step(
        &snapshot(agent),
        agent,
        Some(AutoCompact::Tokens(100_000)),
        "continue",
        ts(20),
        || false,
    )
}

#[test]
fn wrap_up_requires_a_live_participating_open_turn_above_its_line() {
    let candidate = agent();
    assert_eq!(due(&candidate), Some(SoftCompactStep::WrapUp));
    for (name, changed) in [
        ("below threshold", {
            let mut a = candidate.clone();
            a.usage.total_tokens = Some(99_999);
            a
        }),
        ("ended", {
            let mut a = candidate.clone();
            a.ended_at = Some(ts(11));
            a
        }),
        ("resting", {
            let mut a = candidate.clone();
            a.status = AgentStatus::Success;
            a
        }),
        ("waiting", {
            let mut a = candidate.clone();
            a.status = AgentStatus::Waiting;
            a.waiting_since = Some(a.last_activity);
            a
        }),
        ("compacting", {
            let mut a = candidate.clone();
            a.compacting_since = Some(ts(19));
            a
        }),
        ("unprompted compaction", {
            let mut a = candidate.clone();
            a.compacted_awaiting_prompt = Some(ts(19));
            a
        }),
        ("provider child", {
            let mut a = candidate.clone();
            a.parent_agent_id = Some("parent".into());
            a
        }),
        ("unsupported", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("amp");
            a
        }),
    ] {
        assert_eq!(due(&changed), None, "{name}");
    }
    let mut without_pane = snapshot(&candidate);
    std::sync::Arc::make_mut(&mut without_pane.agent_panes).clear();
    assert_eq!(
        due_step(
            &without_pane,
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            "continue",
            ts(20),
            || false
        ),
        None
    );
    assert_eq!(
        due_step(
            &snapshot(&candidate),
            &candidate,
            None,
            "continue",
            ts(20),
            || false
        ),
        None
    );
    assert_eq!(
        due_step(
            &snapshot(&candidate),
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            " \n ",
            ts(20),
            || false
        ),
        None
    );
    let mut child = candidate;
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    assert_eq!(due(&child), Some(SoftCompactStep::WrapUp));
}

#[test]
fn wrap_up_rearms_only_in_a_new_compaction_epoch() {
    let mut candidate = agent();
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::WrappingUp,
    });
    assert_eq!(due(&candidate), None);
    candidate.compaction_count = 1;
    assert_eq!(due(&candidate), Some(SoftCompactStep::WrapUp));
}

#[test]
fn a_mid_turn_compaction_close_does_not_make_continue_due() {
    let mut candidate = agent();
    candidate.usage.total_tokens = Some(20_000);
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::WrappingUp,
    });
    candidate.turn_ended_at = Some(ts(12));
    candidate.compaction_count = 1;
    assert_eq!(
        due(&candidate),
        None,
        "a request close is not a clean turn end"
    );
}

#[test]
fn continue_follows_the_first_later_turn_end_even_if_native_compaction_or_config_changed() {
    let mut candidate = agent();
    candidate.status = AgentStatus::Success;
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::WrappingUp,
    });
    candidate.turn_ended_at = Some(ts(11));
    assert_eq!(
        due(&candidate),
        None,
        "the preceding boundary is not the wrap-up's end"
    );
    candidate.turn_ended_at = Some(ts(12));
    candidate.soft_compact.as_mut().unwrap().progress = SoftCompactProgress::TurnEnded;
    candidate.compaction_count = 1;
    candidate.compacted_awaiting_prompt = Some(ts(13));
    candidate.usage.total_tokens = Some(1);
    assert_eq!(
        due_step(
            &snapshot(&candidate),
            &candidate,
            None,
            "continue",
            ts(20),
            || false
        ),
        Some(SoftCompactStep::Continue)
    );
    candidate.status = AgentStatus::Waiting;
    candidate.waiting_since = Some(candidate.last_activity);
    assert_eq!(due(&candidate), None);
    candidate.status = AgentStatus::Success;
    candidate.soft_compact.as_mut().unwrap().progress = SoftCompactProgress::Continued;
    assert_eq!(due(&candidate), None);
}

fn produce(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
    spawned: bool,
) -> Vec<SoftCompactRequest> {
    let mut requests = Vec::new();
    compact_agents_with(
        snapshot,
        runtime,
        capacities,
        config,
        policy,
        || snapshot.now,
        |request| {
            requests.push(request.clone());
            spawned
        },
    );
    requests
}

#[test]
fn producer_uses_each_seats_line_including_a_pane_backed_child() {
    let dir = tempfile::tempdir().unwrap();
    let mut candidate = agent();
    candidate.profile = Some("coder".to_owned());
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let config = MachineConfig::default();
    let mut policy = SeatPolicy::default();
    policy.profiles.0.insert(
        "coder".to_owned(),
        toml::from_str("agent = 'claude'\nsmart-compact = '100k'").unwrap(),
    );
    policy.subagent_profiles.0.insert(
        "coder".to_owned(),
        toml::from_str("agent = 'claude'\nsmart-compact = '110k'").unwrap(),
    );
    policy.teams.0.insert(
        "forge".to_owned(),
        toml::from_str("[[roles]]\nrole = 'coder'\nprofile = 'coder'\nsmart-compact = 'off'")
            .unwrap(),
    );
    let fire = |seat: &AgentState| {
        produce(
            &snapshot(seat),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            false,
        )
    };
    let requests = fire(&candidate);
    assert_eq!(requests.len(), 1, "the seat overrides the 258k fallback");
    assert_eq!(requests[0].workspace_id, runtime.workspace_id);
    assert_eq!(requests[0].agent_id, candidate.agent_id);
    assert_eq!(requests[0].kind, candidate.kind);
    assert_eq!(requests[0].pane_id, PaneId::from_parts(MuxName::Tmux, "%1"));
    assert_eq!(requests[0].step, SoftCompactStep::WrapUp);
    assert!(!requests[0].label.is_empty());

    candidate.parent_agent_id = Some("parent".into());
    assert!(fire(&candidate).is_empty(), "a provider child is skipped");
    candidate.launch_depth = Some(1);
    assert_eq!(fire(&candidate).len(), 1, "a launched child participates");
    candidate.usage.total_tokens = Some(105_000);
    assert!(fire(&candidate).is_empty(), "the child uses its own table");
    candidate.launch_depth = None;
    candidate.parent_agent_id = None;
    candidate.team = Some("forge".to_owned());
    candidate.role = Some("coder".to_owned());
    assert!(fire(&candidate).is_empty(), "the role's off wins");
    candidate.team = None;
    candidate.profile = None;
    assert!(fire(&candidate).is_empty(), "the fallback is above 105k");
    candidate.usage.total_tokens = Some(258_000);
    assert_eq!(fire(&candidate).len(), 1);
}

#[test]
fn producer_pacing_survives_passes_but_does_not_hold_a_new_step_or_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let mut candidate = agent();
    let fire = |seat: &AgentState, now| {
        let mut snapshot = snapshot(seat);
        snapshot.now = ts(now);
        produce(
            &snapshot,
            &runtime,
            &RoomCapacities::default(),
            &config,
            &SeatPolicy::default(),
            true,
        )
    };
    assert_eq!(fire(&candidate, 20).len(), 1);
    assert!(fire(&candidate, 79).is_empty());
    assert_eq!(fire(&candidate, 80).len(), 1, "a missed steer can retry");
    candidate.compaction_count = 1;
    assert_eq!(fire(&candidate, 81).len(), 1, "a new epoch is independent");
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(81),
        compactions: Some(1),
        progress: SoftCompactProgress::TurnEnded,
    });
    candidate.status = AgentStatus::Success;
    candidate.turn_ended_at = Some(ts(82));
    let requests = fire(&candidate, 82);
    assert_eq!(requests.len(), 1, "wrap-up pacing never delays continue");
    assert_eq!(requests[0].step, SoftCompactStep::Continue);
    assert!(fire(&candidate, 83).is_empty());
    candidate.soft_compact.as_mut().unwrap().steered_at = ts(83);
    candidate.turn_ended_at = Some(ts(84));
    assert_eq!(fire(&candidate, 84).len(), 1, "a new steer is independent");
    candidate.soft_compact.as_mut().unwrap().progress = SoftCompactProgress::Continued;
    assert!(
        fire(&candidate, 200).is_empty(),
        "the mark settles continue"
    );
}

#[test]
fn producer_retries_a_failed_spawn_and_paces_cards_independently() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();
    let mut candidate = agent();
    assert_eq!(
        produce(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            false
        )
        .len(),
        1
    );
    assert_eq!(
        produce(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            true
        )
        .len(),
        1,
        "a failed spawn consumes no pacing window"
    );
    candidate.agent_id = "another-card".into();
    assert_eq!(
        produce(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            true
        )
        .len(),
        1,
        "one card never paces another"
    );
}

#[test]
fn producer_continues_with_the_line_off_but_never_with_blank_continue_text() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let mut candidate = agent();
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::TurnEnded,
    });
    candidate.status = AgentStatus::Success;
    candidate.turn_ended_at = Some(ts(12));
    let snapshot = snapshot(&candidate);
    let mut config = MachineConfig::default();
    config.harness.smart_compact = None;
    config.resume.auto_continue = false;
    assert_eq!(
        produce(
            &snapshot,
            &runtime,
            &RoomCapacities::default(),
            &config,
            &SeatPolicy::default(),
            false
        )
        .first()
        .map(|request| request.step),
        Some(SoftCompactStep::Continue),
        "continue does not depend on either opt-in"
    );
    config.resume.auto_continue_text = " \n ".to_owned();
    assert!(
        produce(
            &snapshot,
            &runtime,
            &RoomCapacities::default(),
            &config,
            &SeatPolicy::default(),
            false
        )
        .is_empty()
    );
}

/// A 5-hour window on `agent`'s login at `used`%, resetting at 500 s.
fn five_hour(agent: &AgentState, used: u8) -> RoomCapacities<'static> {
    let window = crate::agents::RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(ts(500)),
        duration_mins: Some(300),
        ..Default::default()
    };
    std::collections::BTreeMap::from([(
        agent.login_key(),
        crate::agents::ProviderCapacity::from_windows(vec![window]),
    )])
    .into()
}

#[test]
fn a_spent_window_withholds_both_steps_until_the_reset_and_paces_neither() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();
    let wrapping = agent();
    let mut ended = agent();
    ended.status = AgentStatus::Success;
    ended.turn_ended_at = Some(ts(12));
    ended.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::TurnEnded,
    });
    let pacing = || std::fs::read_dir(runtime.live_path("soft-compact")).is_ok();
    for (case, seat, step) in [
        ("wrap-up", &wrapping, SoftCompactStep::WrapUp),
        ("continue", &ended, SoftCompactStep::Continue),
    ] {
        let steps = |now: i64, capacities: &RoomCapacities<'_>| {
            let mut snapshot = snapshot(seat);
            snapshot.now = ts(now);
            produce(&snapshot, &runtime, capacities, &config, &policy, true)
                .into_iter()
                .map(|request| request.step)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            steps(20, &five_hour(seat, 40)),
            [step],
            "{case}: a window with room left"
        );
        std::fs::remove_dir_all(runtime.live_path("soft-compact")).unwrap();
        assert!(
            steps(20, &five_hour(seat, 100)).is_empty(),
            "{case}: withheld at a spent window"
        );
        assert!(!pacing(), "{case}: a withheld step paces nothing");
        assert_eq!(
            steps(600, &five_hour(seat, 100)),
            [step],
            "{case}: proposed again once the reset passed"
        );
        std::fs::remove_dir_all(runtime.live_path("soft-compact")).unwrap();
        assert_eq!(
            steps(20, &RoomCapacities::default()),
            [step],
            "{case}: no reading"
        );
        std::fs::remove_dir_all(runtime.live_path("soft-compact")).unwrap();
        let paid = |extra, entitlement| {
            five_hour(seat, 100).with_credits(CreditsCache::with_extra_credits(
                seat.login_key(),
                extra,
                entitlement,
                ts(20),
            ))
        };
        assert_eq!(
            steps(20, &paid(ExtraCredits::Unlimited, Entitlement::Ok)),
            [step],
            "{case}: usable paid extra usage carries the spent window"
        );
        std::fs::remove_dir_all(runtime.live_path("soft-compact")).unwrap();
        for (why, extra, entitlement) in [
            ("disabled", ExtraCredits::Disabled, Entitlement::Ok),
            (
                "lapsed",
                ExtraCredits::Unlimited,
                Entitlement::Lapsed { since_ms: 1 },
            ),
        ] {
            assert!(
                steps(20, &paid(extra, entitlement)).is_empty(),
                "{case}: paid extra usage {why} carries nothing"
            );
        }
        let mut opus = seat.clone();
        opus.model = Some("claude-opus-4-6".to_owned());
        let sub_cap = crate::agents::RateLimitWindow {
            scope: Some(crate::agents::RateLimitWindowScope {
                id: "model:opus".into(),
                label: "Opus".into(),
            }),
            used_percentage: Some(100),
            resets_at: Some(ts(500)),
            duration_mins: Some(10_080),
            ..Default::default()
        };
        let capped = RoomCapacities::from(std::collections::BTreeMap::from([(
            opus.login_key(),
            crate::agents::ProviderCapacity::from_windows(vec![sub_cap]),
        )]))
        .with_credits(CreditsCache::with_extra_credits(
            opus.login_key(),
            ExtraCredits::Unlimited,
            Entitlement::Ok,
            ts(20),
        ));
        let mut snapshot = snapshot(&opus);
        snapshot.now = ts(20);
        assert!(
            produce(&snapshot, &runtime, &capped, &config, &policy, true).is_empty(),
            "{case}: the model's own sub-cap, even with usable paid extra usage"
        );
    }
    assert_eq!(
        ended.soft_compact.map(|mark| mark.progress),
        Some(SoftCompactProgress::TurnEnded),
        "the decision leaves the mark owed"
    );
}
