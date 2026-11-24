use super::*;
use crate::agents::credits::CreditsCache;
use crate::agents::state::SoftCompactMark;
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

fn due(agent: &AgentState) -> bool {
    reminder_due(
        &snapshot(agent),
        agent,
        Some(AutoCompact::Tokens(100_000)),
        "continue",
        ts(20),
        || false,
    )
}

#[test]
fn reminder_requires_a_live_participating_open_turn_above_its_line() {
    let candidate = agent();
    assert!(due(&candidate));
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
        ("continue owed", {
            let mut a = candidate.clone();
            a.soft_compact = Some(SoftCompactMark {
                compactions: None,
                continue_owed: Some(crate::agents::state::OwedContinue {
                    message_id: crate::ids::MessageId::new(),
                    queued_at: ts(1),
                }),
            });
            a
        }),
    ] {
        assert!(!due(&changed), "{name}");
    }
    let mut without_pane = snapshot(&candidate);
    std::sync::Arc::make_mut(&mut without_pane.agent_panes).clear();
    assert!(!reminder_due(
        &without_pane,
        &candidate,
        Some(AutoCompact::Tokens(100_000)),
        "continue",
        ts(20),
        || false
    ));
    assert!(!reminder_due(
        &snapshot(&candidate),
        &candidate,
        None,
        "continue",
        ts(20),
        || false
    ));
    assert!(
        !reminder_due(
            &snapshot(&candidate),
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            " \n ",
            ts(20),
            || false
        ),
        "blank continue text withholds the reminder"
    );
    assert!(
        !reminder_due(
            &snapshot(&candidate),
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            "continue",
            ts(20),
            || true
        ),
        "a blocked login withholds the reminder"
    );
    let mut child = candidate;
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    assert!(due(&child));
}

#[test]
fn reminder_rearms_only_in_a_new_compaction_epoch() {
    let mut candidate = agent();
    candidate.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        continue_owed: None,
    });
    assert!(!due(&candidate));
    candidate.compaction_count = 1;
    assert!(due(&candidate), "a mid-turn compaction opens a new epoch");
    candidate.usage.total_tokens = Some(20_000);
    assert!(!due(&candidate), "the new epoch waits for its own line");
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
fn producer_pacing_survives_passes_but_does_not_hold_a_new_epoch() {
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
    assert_eq!(fire(&candidate, 80).len(), 1, "a missed reminder can retry");
    candidate.compaction_count = 1;
    let requests = fire(&candidate, 81);
    assert_eq!(requests.len(), 1, "a new epoch is independent");
    assert_eq!(requests[0].step, SoftCompactStep::WrapUp);
    candidate.soft_compact = Some(SoftCompactMark {
        compactions: Some(1),
        continue_owed: None,
    });
    assert!(
        fire(&candidate, 200).is_empty(),
        "a sent reminder settles its epoch"
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
fn producer_sends_nothing_with_the_line_off_or_blank_continue_text() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let snapshot = snapshot(&agent());
    let produced = |config: &MachineConfig| {
        produce(
            &snapshot,
            &runtime,
            &RoomCapacities::default(),
            config,
            &SeatPolicy::default(),
            false,
        )
    };
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    assert_eq!(produced(&config).len(), 1);
    config.resume.auto_continue_text = " \n ".to_owned();
    assert!(produced(&config).is_empty(), "blank continue text");
    config.resume.auto_continue_text = "continue".to_owned();
    config.harness.smart_compact = Some(AutoCompact::Tokens(200_000));
    assert!(produced(&config).is_empty(), "below the line");
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
fn a_spent_window_withholds_the_reminder_until_the_reset_and_paces_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();
    let wrapping = agent();
    let pacing = || std::fs::read_dir(runtime.live_path("soft-compact")).is_ok();
    for (case, seat, step) in [("reminder", &wrapping, SoftCompactStep::WrapUp)] {
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
}

#[test]
fn reminder_stays_suppressed_on_withheld_lagging_sidecar_through_first_prompt_then_rearms() {
    use crate::agents::context::{AgentContext, AgentTokenUsage};
    use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
    use crate::ids::AgentSessionId;
    use crate::store::writer::AgentLifecycleIntent;
    use crate::store::{Store, agent_context};

    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths =
        crate::disk::paths::StatePaths::under(workspace_id.clone(), &dir.path().join("state"))
            .unwrap();
    let runtime = RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("antigravity");
    let session = AgentSessionId::from("sess-agy");

    let write_sidecar = |used: u64| {
        let mut ctx = AgentContext::new("antigravity", Timestamp::now());
        ctx.tokens = Some(AgentTokenUsage {
            context_window_size: Some(1_000_000),
            used_percentage: Some(((used * 100) / 1_000_000) as u8),
            remaining_percentage: Some(100 - ((used * 100) / 1_000_000) as u8),
            current_context_tokens: Some(used),
            current_usage: None,
            session_usage: None,
        });
        agent_context::write(
            store.runtime_paths(),
            "antigravity",
            session.as_str(),
            &ctx,
            None,
        )
        .unwrap();
    };
    let append = |event: &str, signal: LifecycleSignal| {
        let obs = AgentLifecycleObservation::new(Some(session.clone()), signal);
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind.clone(),
                event_name: event,
                observation: &obs,
                spawned_subagents: &[],
            })
            .unwrap();
    };

    write_sidecar(150_000);
    append(
        "PreInvocation",
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    append("PreInvocation", LifecycleSignal::Compacting);
    append(
        "Stop",
        LifecycleSignal::CompactionEnded {
            auto: Some(false),
            failed: false,
        },
    );
    // First prompt clears `compacted_awaiting_prompt`, leaving only the retired-reading guard.
    append(
        "PreInvocation",
        LifecycleSignal::TurnStarted { turn_id: None },
    );

    let read_agent = || {
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|agent| agent.agent_id == session)
            .cloned()
            .expect("agent row")
    };

    let withheld = read_agent();
    assert!(withheld.compacted_awaiting_prompt.is_none());
    assert!(
        !due(&withheld),
        "stale pre-compaction sidecar reading must not re-fire reminder after first prompt"
    );

    write_sidecar(14_038);
    let post = read_agent();
    assert!(
        !due(&post),
        "low post-compaction reading stays below threshold"
    );

    write_sidecar(120_000);
    let crossed = read_agent();
    assert!(
        due(&crossed),
        "genuine crossing above threshold re-arms reminder"
    );
}
