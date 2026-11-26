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
    .is_due()
}

fn verdict(agent: &AgentState, threshold: AutoCompact) -> ReminderVerdict {
    reminder_due(
        &snapshot(agent),
        agent,
        Some(threshold),
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
        ("no hook context", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("kimi");
            a
        }),
        ("continue owed", {
            let mut a = candidate.clone();
            a.soft_compact = Some(SoftCompactMark {
                compactions: None,
                continue_owed: Some(crate::agents::state::OwedContinue {
                    message_id: crate::ids::MessageId::new(),
                    queued_at: ts(1),
                    past_stop: false,
                }),
                ..SoftCompactMark::default()
            });
            a
        }),
    ] {
        assert!(!due(&changed), "{name}");
    }
    let mut without_pane = snapshot(&candidate);
    std::sync::Arc::make_mut(&mut without_pane.agent_panes).clear();
    assert!(
        !reminder_due(
            &without_pane,
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            "continue",
            ts(20),
            || false
        )
        .is_due()
    );
    assert!(
        !reminder_due(
            &snapshot(&candidate),
            &candidate,
            None,
            "continue",
            ts(20),
            || false
        )
        .is_due()
    );
    assert!(
        !reminder_due(
            &snapshot(&candidate),
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            " \n ",
            ts(20),
            || false
        )
        .is_due(),
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
        )
        .is_due(),
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
        rung: Some(2),
        ..SoftCompactMark::default()
    });
    assert_eq!(
        verdict(&candidate, AutoCompact::Tokens(100_000)),
        ReminderVerdict::NotDue
    );
    candidate.compaction_count = 1;
    assert_eq!(
        verdict(&candidate, AutoCompact::Tokens(100_000)),
        ReminderVerdict::WithheldByGuard,
        "a guarded kind without a low-water reading under the line stays withheld"
    );
    candidate.soft_compact.as_mut().unwrap().low_water_tokens = Some(20_000);
    assert_eq!(
        verdict(&candidate, AutoCompact::Tokens(100_000)),
        ReminderVerdict::Due(2),
        "a mid-turn compaction with a low-water reading under the line opens a new epoch"
    );
    candidate.usage.total_tokens = Some(20_000);
    assert_eq!(
        verdict(&candidate, AutoCompact::Tokens(100_000)),
        ReminderVerdict::NotDue,
        "the new epoch waits for its own line"
    );
}

/// Runs one host pass, then claims what it armed for each row by its session key, as a root
/// post-tool hook without `RIMZ_AGENT_ID` would.
fn produce(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
) -> Vec<SoftCompactClaim> {
    produce_with_diag(
        snapshot,
        runtime,
        capacities,
        config,
        policy,
        &DiagSink::disabled(),
    )
}

fn produce_with_diag(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
    diag: &DiagSink,
) -> Vec<SoftCompactClaim> {
    compact_agents(snapshot, runtime, capacities, config, policy, diag);
    snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            claim_pending(runtime, &agent.kind, None, Some(agent.agent_id.as_str()))
        })
        .collect()
}

fn line_config(line: AutoCompact) -> MachineConfig {
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(line);
    config
}

#[test]
fn producer_uses_each_seats_line_including_a_pane_backed_child() {
    let dir = tempfile::tempdir().unwrap();
    let mut candidate = agent();
    candidate.usage.total_tokens = Some(105_000);
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
        let claims = produce(
            &snapshot(seat),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
        );
        // Each case starts unarmed: a claim the fold has not read keeps its rung armed.
        let _ = std::fs::remove_dir_all(runtime.live_path("soft-compact"));
        claims
    };
    let claims = fire(&candidate);
    assert_eq!(
        claims,
        [SoftCompactClaim {
            agent_id: candidate.agent_id.clone(),
            compactions: 0,
            rung: 1,
            threshold: AutoCompact::Tokens(100_000),
            label: claims[0].label.clone(),
            occupied_tokens: Some(105_000),
        }],
        "the seat overrides the 258k fallback"
    );
    assert!(
        claims[0]
            .label
            .as_ref()
            .is_some_and(|label| !label.is_empty())
    );

    candidate.parent_agent_id = Some("parent".into());
    assert!(fire(&candidate).is_empty(), "a provider child is skipped");
    candidate.launch_depth = Some(1);
    candidate.usage.total_tokens = Some(115_000);
    assert_eq!(fire(&candidate).len(), 1, "a launched child participates");
    candidate.usage.total_tokens = Some(109_000);
    assert!(fire(&candidate).is_empty(), "the child uses its own table");
    candidate.launch_depth = None;
    candidate.parent_agent_id = None;
    candidate.team = Some("forge".to_owned());
    candidate.role = Some("coder".to_owned());
    assert!(fire(&candidate).is_empty(), "the role's off wins");
    candidate.team = None;
    candidate.profile = None;
    assert!(fire(&candidate).is_empty(), "the fallback is above 109k");
    candidate.usage.total_tokens = Some(258_000);
    assert_eq!(fire(&candidate).len(), 1);
}

#[test]
fn producer_arms_each_rung_once_and_clears_a_stale_pending() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let config = line_config(AutoCompact::Tokens(100_000));
    let pass = |seat: &AgentState| {
        compact_agents(
            &snapshot(seat),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &SeatPolicy::default(),
            &DiagSink::disabled(),
        );
    };
    let claim = |seat: &AgentState| {
        claim_pending(&runtime, &seat.kind, None, Some(seat.agent_id.as_str()))
            .map(|claim| (claim.compactions, claim.rung))
    };
    let mut candidate = agent();
    candidate.usage.total_tokens = Some(105_000);
    pass(&candidate);
    pass(&candidate);
    assert_eq!(claim(&candidate), Some((0, 1)), "rung 1 at the line");
    assert_eq!(claim(&candidate), None, "a second hook attaches nothing");
    pass(&candidate);
    assert_eq!(
        claim(&candidate),
        None,
        "a claim the fold has not read yet arms nothing again"
    );
    candidate.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        rung: Some(1),
        ..SoftCompactMark::default()
    });
    candidate.usage.total_tokens = Some(110_000);
    pass(&candidate);
    assert_eq!(
        claim(&candidate),
        Some((0, 2)),
        "rung 2 at 110% of the line"
    );
    candidate.soft_compact.as_mut().unwrap().rung = Some(2);
    candidate.usage.total_tokens = Some(150_000);
    pass(&candidate);
    assert_eq!(claim(&candidate), None, "both rungs spent in this epoch");

    // An armed rung nobody claimed is removed once the epoch moves under the line.
    candidate.compaction_count = 1;
    candidate.soft_compact = None;
    pass(&candidate);
    candidate.compaction_count = 2;
    candidate.usage.total_tokens = Some(20_000);
    pass(&candidate);
    assert_eq!(claim(&candidate), None, "a stale pending is removed");
    candidate.usage.total_tokens = Some(130_000);
    pass(&candidate);
    assert_eq!(
        claim(&candidate),
        Some((2, 2)),
        "a jump past both lines arms one reminder at rung 2"
    );
}

#[test]
fn claim_pending_takes_once_by_launch_or_session_key() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let config = line_config(AutoCompact::Tokens(100_000));
    let mut candidate = agent();
    candidate.launch_id = Some("launch-1".into());
    let pass = |seat: &AgentState| {
        compact_agents(
            &snapshot(seat),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &SeatPolicy::default(),
            &DiagSink::disabled(),
        );
    };
    pass(&candidate);
    assert!(
        claim_pending(&runtime, &candidate.kind, Some("launch-2"), Some("session")).is_none(),
        "another launch of the same session key finds nothing"
    );
    assert!(
        claim_pending(
            &runtime,
            &candidate.kind,
            Some("launch-1"),
            Some("native-child")
        )
        .is_none(),
        "a native child sharing the launch's env takes nothing"
    );
    assert_eq!(
        claim_pending(&runtime, &candidate.kind, Some("launch-1"), None)
            .map(|claim| claim.agent_id),
        Some(candidate.agent_id.clone()),
        "the launch id finds the row when the payload names no session"
    );
    assert!(
        claim_pending(&runtime, &candidate.kind, Some("launch-1"), Some("session")).is_none(),
        "taken once"
    );

    // A launched agent whose hook env carries no launch id finds it by the session key.
    std::fs::remove_dir_all(runtime.live_path("soft-compact")).unwrap();
    pass(&candidate);
    assert!(claim_pending(&runtime, &candidate.kind, Some(""), Some("session")).is_some());

    // A pending file that cannot be read attaches nothing and is not retried.
    std::fs::remove_dir_all(runtime.live_path("soft-compact")).unwrap();
    pass(&candidate);
    let dir = runtime.live_path("soft-compact");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().ends_with("-pending.json") {
            std::fs::write(&path, "not json").unwrap();
        }
    }
    assert!(claim_pending(&runtime, &candidate.kind, Some("launch-1"), None).is_none());
    assert!(claim_pending(&runtime, &candidate.kind, Some("launch-1"), None).is_none());
    assert!(claim_pending(&runtime, &candidate.kind, None, None).is_none());
}

#[test]
fn producer_arms_cards_independently_and_nothing_for_a_kind_without_hook_context() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let config = line_config(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();
    let mut candidate = agent();
    assert_eq!(
        produce(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
        )
        .len(),
        1
    );
    candidate.agent_id = "another-card".into();
    assert_eq!(
        produce(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
        )
        .len(),
        1,
        "one card never paces another"
    );
    candidate.agent_id = "hookless".into();
    candidate.kind = AgentKind::new_unchecked("kimi");
    assert!(
        produce(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
        )
        .is_empty()
    );
    assert!(
        !std::fs::read_dir(runtime.live_path("soft-compact"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&crate::store::sidecar::digest("antigravity", "hookless"))),
        "no pending file for a kind without hook context"
    );
}

#[test]
fn producer_sends_nothing_with_the_line_off_or_blank_continue_text() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let snapshot = snapshot(&agent());
    let produced = |config: &MachineConfig| {
        let claims = produce(
            &snapshot,
            &runtime,
            &RoomCapacities::default(),
            config,
            &SeatPolicy::default(),
        );
        let _ = std::fs::remove_dir_all(runtime.live_path("soft-compact"));
        claims
    };
    let mut config = line_config(AutoCompact::Tokens(100_000));
    assert_eq!(produced(&config).len(), 1);
    config.resume.auto_continue_text = " \n ".to_owned();
    assert!(produced(&config).is_empty(), "blank continue text");
    config.resume.auto_continue_text = "continue".to_owned();
    config.harness.smart_compact = Some(AutoCompact::Tokens(200_000));
    assert!(produced(&config).is_empty(), "below the line");
}

#[test]
fn second_rung_fires_at_ten_percent_past_the_line_below_the_ceiling() {
    assert_eq!(
        second_line(AutoCompact::Tokens(200_000)),
        Some(AutoCompact::Tokens(220_000))
    );
    assert_eq!(
        second_line(AutoCompact::Percent(70)),
        Some(AutoCompact::Percent(77))
    );
    assert_eq!(
        second_line(AutoCompact::Percent(90)),
        Some(AutoCompact::Percent(99))
    );
    assert_eq!(second_line(AutoCompact::Percent(91)), None, "past 100%");
    for (line, second) in [(5, 6), (9, 10), (10, 11)] {
        assert_eq!(
            second_line(AutoCompact::Percent(line)),
            Some(AutoCompact::Percent(second)),
            "a small percent line keeps a second line above it"
        );
    }
    assert_eq!(
        second_line(AutoCompact::Tokens(5)),
        Some(AutoCompact::Tokens(6))
    );

    let mut candidate = agent();
    let line = AutoCompact::Tokens(100_000);
    candidate.usage.total_tokens = Some(105_000);
    assert_eq!(verdict(&candidate, line), ReminderVerdict::Due(1));
    candidate.usage.total_tokens = Some(110_000);
    assert_eq!(
        verdict(&candidate, line),
        ReminderVerdict::Due(2),
        "a fill past both lines owes one reminder, at rung 2"
    );
    candidate.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        ..SoftCompactMark::default()
    });
    assert_eq!(
        verdict(&candidate, line),
        ReminderVerdict::Due(2),
        "a legacy wrap-up spent rung 1 only"
    );
    candidate.usage.total_tokens = Some(105_000);
    assert_eq!(verdict(&candidate, line), ReminderVerdict::NotDue);
    candidate.soft_compact.as_mut().unwrap().rung = Some(2);
    candidate.usage.total_tokens = Some(200_000);
    assert_eq!(
        verdict(&candidate, line),
        ReminderVerdict::NotDue,
        "a claimed rung 2 spends the epoch"
    );
    candidate.compaction_count = 1;
    candidate.soft_compact.as_mut().unwrap().low_water_tokens = Some(20_000);
    assert_eq!(
        verdict(&candidate, line),
        ReminderVerdict::Due(2),
        "a compaction re-arms both rungs"
    );

    // Rung 2 at or past the row's auto-compact ceiling never fires.
    let mut capped = agent();
    capped.usage.total_tokens = Some(115_000);
    capped.record = Some(Box::new(crate::agents::LaunchRecord {
        auto_compact: Some(110_000),
        ..Default::default()
    }));
    assert_eq!(verdict(&capped, line), ReminderVerdict::Due(1));
    capped.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        rung: Some(1),
        ..SoftCompactMark::default()
    });
    assert_eq!(verdict(&capped, line), ReminderVerdict::NotDue);

    // Percent lines take the same rungs over the resolved window.
    let mut pct = agent();
    pct.usage.context_window = Some(200_000);
    pct.usage.fresh_input_tokens = Some(150_000);
    assert_eq!(
        verdict(&pct, AutoCompact::Percent(70)),
        ReminderVerdict::Due(1)
    );
    pct.usage.fresh_input_tokens = Some(154_000);
    assert_eq!(
        verdict(&pct, AutoCompact::Percent(70)),
        ReminderVerdict::Due(2)
    );
    pct.usage.fresh_input_tokens = Some(198_000);
    assert_eq!(
        verdict(&pct, AutoCompact::Percent(95)),
        ReminderVerdict::Due(1),
        "no second line past 100%"
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
fn a_spent_window_withholds_the_reminder_until_the_reset_and_paces_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();
    let wrapping = agent();
    let pacing = || std::fs::read_dir(runtime.live_path("soft-compact")).is_ok();
    for (case, seat, step) in [("reminder", &wrapping, 2)] {
        let steps = |now: i64, capacities: &RoomCapacities<'_>| {
            let mut snapshot = snapshot(seat);
            snapshot.now = ts(now);
            produce(&snapshot, &runtime, capacities, &config, &policy)
                .into_iter()
                .map(|claim| claim.rung)
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
            produce(&snapshot, &runtime, &capped, &config, &policy).is_empty(),
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
            false,
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
            echo: false,
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

    // This pins the line's own reading, which `reminder_due` gates on.
    let due = |row: &AgentState| AutoCompact::Tokens(100_000).triggered(row);
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

#[test]
fn guarded_kinds_match_lifecycle_reports_fill_declaration() {
    let expected: std::collections::BTreeSet<&str> =
        ["claude", "codex", "omp", "opencode", "pi"].into();
    let actual: std::collections::BTreeSet<&str> = crate::agents::all_definitions()
        .filter(|definition| definition.spec().capabilities.lifecycle_reports_fill)
        .map(|definition| definition.spec().kind)
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn fill_guard_verdicts_cover_guarded_percent_token_unguarded_clear_and_continuation() {
    for kind in ["claude", "codex", "pi", "opencode"] {
        let mut candidate = agent();
        candidate.kind = AgentKind::new_unchecked(kind);
        candidate.usage.context_window = Some(200_000);
        candidate.usage.total_tokens = Some(160_000);
        candidate.usage.fresh_input_tokens = Some(160_000);
        candidate.compaction_count = 1;
        candidate.soft_compact = Some(SoftCompactMark {
            compactions: Some(0),
            ..SoftCompactMark::default()
        });

        // No low-water reading since reminder withholds for both token and percent lines.
        assert_eq!(
            verdict(&candidate, AutoCompact::Tokens(100_000)),
            ReminderVerdict::WithheldByGuard,
            "{kind}: blank low-water tokens"
        );
        assert_eq!(
            verdict(&candidate, AutoCompact::Percent(70)),
            ReminderVerdict::WithheldByGuard,
            "{kind}: blank low-water percent"
        );

        // At or over the line still withholds.
        candidate.soft_compact.as_mut().unwrap().low_water_tokens = Some(100_000);
        assert_eq!(
            verdict(&candidate, AutoCompact::Tokens(100_000)),
            ReminderVerdict::WithheldByGuard,
            "{kind}: at token line"
        );
        candidate.soft_compact.as_mut().unwrap().low_water_tokens = Some(140_000);
        assert_eq!(
            verdict(&candidate, AutoCompact::Percent(70)),
            ReminderVerdict::WithheldByGuard,
            "{kind}: at percent line"
        );

        // Strictly under the line re-arms.
        candidate.soft_compact.as_mut().unwrap().low_water_tokens = Some(99_999);
        assert_eq!(
            verdict(&candidate, AutoCompact::Tokens(100_000)),
            ReminderVerdict::Due(2),
            "{kind}: under token line"
        );
        candidate.soft_compact.as_mut().unwrap().low_water_tokens = Some(139_999);
        assert_eq!(
            verdict(&candidate, AutoCompact::Percent(70)),
            ReminderVerdict::Due(2),
            "{kind}: under percent line"
        );

        // Percent-only low-water reading (Pi/OMP with no token figure).
        candidate.soft_compact.as_mut().unwrap().low_water_tokens = None;
        candidate.soft_compact.as_mut().unwrap().low_water_pct = Some(70);
        assert_eq!(
            verdict(&candidate, AutoCompact::Percent(70)),
            ReminderVerdict::WithheldByGuard,
            "{kind}: at percent-only line"
        );
        candidate.soft_compact.as_mut().unwrap().low_water_pct = Some(69);
        assert_eq!(
            verdict(&candidate, AutoCompact::Percent(70)),
            ReminderVerdict::Due(2),
            "{kind}: under percent-only line"
        );

        // Compaction-blocked login returns NotDue, not WithheldByGuard.
        candidate.soft_compact.as_mut().unwrap().low_water_pct = None;
        assert_eq!(
            reminder_due(
                &snapshot(&candidate),
                &candidate,
                Some(AutoCompact::Tokens(100_000)),
                "continue",
                ts(20),
                || true,
            ),
            ReminderVerdict::NotDue,
            "{kind}: blocked login is NotDue"
        );

        // Continuation row (compactions: None) and cleared mark (None) bypass the guard.
        candidate.soft_compact = Some(SoftCompactMark::default());
        assert_eq!(
            verdict(&candidate, AutoCompact::Tokens(100_000)),
            ReminderVerdict::Due(2),
            "{kind}: continuation mark re-arms without low-water"
        );
        candidate.soft_compact = None;
        assert_eq!(
            verdict(&candidate, AutoCompact::Tokens(100_000)),
            ReminderVerdict::Due(2),
            "{kind}: cleared mark re-arms without low-water"
        );
    }

    // Unguarded participating kinds re-arm on the count alone.
    for kind in ["droid", "grok", "qwen"] {
        let mut candidate = agent();
        candidate.kind = AgentKind::new_unchecked(kind);
        candidate.compaction_count = 1;
        candidate.soft_compact = Some(SoftCompactMark {
            compactions: Some(0),
            ..SoftCompactMark::default()
        });
        assert_eq!(
            verdict(&candidate, AutoCompact::Tokens(100_000)),
            ReminderVerdict::Due(2),
            "{kind}: unguarded kind re-arms on count alone"
        );
    }
}

#[test]
fn host_pass_records_one_contained_diagnostic_per_agent_and_count_when_guard_withholds() {
    use crate::diag::record::{DiagEnvelope, DiagEvent};

    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), &dir.path().join("runtime")).unwrap();
    let state_root = dir.path().join("state");
    let diag = DiagSink::under(state_root, workspace, "rimz-test", None);
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();

    let mut candidate = agent();
    candidate.name = Some("scout".to_owned());
    candidate.compaction_count = 1;
    candidate.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        low_water_tokens: Some(115_000),
        ..SoftCompactMark::default()
    });

    // A disabled sink writes no dedupe cache, so the host's enabled sink still records once.
    assert!(
        produce_with_diag(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            &DiagSink::disabled(),
        )
        .is_empty()
    );

    // Repeated passes (even past the 30s diag limiter window) emit only one record for count 1.
    for seconds in [20, 40, 120] {
        let mut snap = snapshot(&candidate);
        snap.now = ts(seconds);
        assert!(
            produce_with_diag(
                &snap,
                &runtime,
                &RoomCapacities::default(),
                &config,
                &policy,
                &diag,
            )
            .is_empty()
        );
    }

    let read_records = || -> Vec<DiagEnvelope> {
        let path = diag.log_path().unwrap();
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    let records = read_records();
    assert_eq!(
        records.len(),
        1,
        "one record per agent and count: {records:?}"
    );
    assert_eq!(
        records[0].event,
        DiagEvent::SoftCompactReminderWithheld {
            agent_kind: AgentKind::new_unchecked("claude"),
            agent_id: candidate.agent_id.clone(),
            agent_name: Some("scout".to_owned()),
            compaction_count: 1,
            low_water_tokens: Some(115_000),
            low_water_pct: None,
            threshold: "100000 tokens".to_owned(),
        }
    );

    // Moving to compaction count 2 emits one more record for that count.
    candidate.compaction_count = 2;
    candidate.soft_compact.as_mut().unwrap().low_water_tokens = None;
    let mut snap = snapshot(&candidate);
    snap.now = ts(180);
    assert!(
        produce_with_diag(
            &snap,
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            &diag,
        )
        .is_empty()
    );
    let records = read_records();
    assert_eq!(records.len(), 2, "new count emits once more: {records:?}");
    assert_eq!(
        records[1].event,
        DiagEvent::SoftCompactReminderWithheld {
            agent_kind: AgentKind::new_unchecked("claude"),
            agent_id: candidate.agent_id.clone(),
            agent_name: Some("scout".to_owned()),
            compaction_count: 2,
            low_water_tokens: None,
            low_water_pct: None,
            threshold: "100000 tokens".to_owned(),
        }
    );
}

#[test]
fn guard_cache_or_diag_failures_leave_verdict_unchanged_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), &dir.path().join("runtime")).unwrap();
    let state_root = dir.path().join("state");
    let diag = DiagSink::under(state_root, workspace, "rimz-test", None);
    let mut config = MachineConfig::default();
    config.harness.smart_compact = Some(AutoCompact::Tokens(100_000));
    let policy = SeatPolicy::default();

    let mut candidate = agent();
    candidate.compaction_count = 1;
    candidate.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        ..SoftCompactMark::default()
    });

    // 1. Corrupt withheld cache file: still withholds, spawns nothing, emits diag.
    let cache_path = runtime.live_path("soft-compact").join(format!(
        "{}-withheld.json",
        crate::store::sidecar::digest(candidate.kind.as_str(), candidate.agent_id.as_str())
    ));
    std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    std::fs::write(&cache_path, "not-valid-json").unwrap();
    assert!(
        produce_with_diag(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            &diag,
        )
        .is_empty()
    );
    assert!(diag.log_path().unwrap().exists());

    // 2. Unwritable cache path (directory at cache file path) and unwritable diag log path
    //    (directory at the sink's log path): still withholds, spawns nothing, never panics.
    std::fs::remove_file(&cache_path).unwrap();
    std::fs::create_dir_all(&cache_path).unwrap();
    let log_path = diag.log_path().unwrap();
    std::fs::remove_file(&log_path).unwrap();
    std::fs::create_dir_all(&log_path).unwrap();
    assert!(
        produce_with_diag(
            &snapshot(&candidate),
            &runtime,
            &RoomCapacities::default(),
            &config,
            &policy,
            &diag,
        )
        .is_empty()
    );
}

#[test]
fn ceiling_due_checks_every_condition_and_excludes_native_and_unsupported_kinds() {
    let now = ts(10_000);
    let mut candidate = agent();
    candidate.kind = AgentKind::new_unchecked("antigravity");
    candidate.record = Some(Box::new(crate::agents::LaunchRecord {
        auto_compact: Some(372_000),
        ..Default::default()
    }));
    candidate.usage.total_tokens = Some(372_000);
    candidate.usage.context_window = Some(1_000_000);

    let check = |row: &AgentState| ceiling_due(&snapshot(row), row, "continue", now, || false);

    assert_eq!(check(&candidate), Some(372_000));

    for (name, changed) in [
        ("below ceiling", {
            let mut a = candidate.clone();
            a.usage.total_tokens = Some(371_999);
            a
        }),
        ("no launch record", {
            let mut a = candidate.clone();
            a.record = None;
            a
        }),
        ("launch record without auto_compact", {
            let mut a = candidate.clone();
            a.record = Some(Box::new(crate::agents::LaunchRecord::default()));
            a
        }),
        ("ended", {
            let mut a = candidate.clone();
            a.ended_at = Some(ts(9_000));
            a
        }),
        ("resting", {
            let mut a = candidate.clone();
            a.status = AgentStatus::Success;
            a
        }),
        ("parked", {
            let mut a = candidate.clone();
            a.phase = crate::agents::TurnPhase::Parked;
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
            a.compacting_since = Some(ts(9_990));
            a
        }),
        ("unprompted compaction", {
            let mut a = candidate.clone();
            a.compacted_awaiting_prompt = Some(ts(9_000));
            a
        }),
        ("provider subagent", {
            let mut a = candidate.clone();
            a.parent_agent_id = Some("parent".into());
            a
        }),
        ("inside 10m cooldown", {
            let mut a = candidate.clone();
            a.compacted_at = Some(ts(10_000 - CEILING_COOLDOWN.as_secs() as i64 + 1));
            a
        }),
        ("native-enforced claude", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("claude");
            a
        }),
        ("native-enforced codex", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("codex");
            a
        }),
        ("no compact command amp", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("amp");
            a
        }),
        ("no compaction signal hermes", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("hermes");
            a
        }),
        ("model window below the level", {
            let mut a = candidate.clone();
            a.usage.context_window = Some(200_000);
            a
        }),
    ] {
        assert_eq!(check(&changed), None, "{name}");
    }
    let mut pi = candidate.clone();
    pi.kind = AgentKind::new_unchecked("pi");
    assert_eq!(check(&pi), Some(372_000), "pi has no native window");
    for kind in ["copilot", "kimi", "omp", "droid"] {
        let mut row = candidate.clone();
        row.kind = AgentKind::new_unchecked(kind);
        assert_eq!(
            check(&row),
            None,
            "{kind}'s message keys were never checked"
        );
    }

    let mut at_cooldown = candidate.clone();
    at_cooldown.compacted_at = Some(ts(10_000 - CEILING_COOLDOWN.as_secs() as i64));
    assert_eq!(
        check(&at_cooldown),
        Some(372_000),
        "exact cool-down boundary re-arms"
    );

    let mut spent_without_low_water = candidate.clone();
    spent_without_low_water.compaction_count = 1;
    spent_without_low_water.soft_compact = Some(SoftCompactMark {
        compactions: Some(0),
        low_water_tokens: None,
        low_water_pct: None,
        continue_owed: None,
        rung: None,
    });
    assert_eq!(
        check(&spent_without_low_water),
        Some(372_000),
        "ceiling watch does not read the reminder's low-water mark"
    );

    let mut without_pane = snapshot(&candidate);
    std::sync::Arc::make_mut(&mut without_pane.agent_panes).clear();
    assert_eq!(
        ceiling_due(&without_pane, &candidate, "continue", now, || false),
        None
    );
    assert_eq!(
        ceiling_due(&snapshot(&candidate), &candidate, " \n ", now, || false),
        None,
        "blank continue text withholds ceiling enforcement"
    );
    assert_eq!(
        ceiling_due(&snapshot(&candidate), &candidate, "continue", now, || true),
        None,
        "blocked login withholds ceiling enforcement"
    );

    let mut child = candidate.clone();
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    assert_eq!(
        check(&child),
        Some(372_000),
        "pane-backed launched child qualifies"
    );

    // Host pass pacing: spawns once, throttles inside RESPAWN_THROTTLE, respawns on new epoch.
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let config = MachineConfig::default();
    let mut fired = Vec::new();
    let mut snap = snapshot(&candidate);
    snap.now = now;
    enforce_ceiling_agents_with(
        &snap,
        &runtime,
        &RoomCapacities::default(),
        &config,
        || now,
        |req| {
            fired.push(req.clone());
            true
        },
    );
    assert_eq!(fired.len(), 1);
    enforce_ceiling_agents_with(
        &snap,
        &runtime,
        &RoomCapacities::default(),
        &config,
        || now + jiff::SignedDuration::from_secs(30),
        |req| {
            fired.push(req.clone());
            true
        },
    );
    assert_eq!(fired.len(), 1, "pacing throttles within 60s");
    let mut next_epoch = candidate.clone();
    next_epoch.compaction_count = 1;
    let mut next_snap = snapshot(&next_epoch);
    next_snap.now = now + jiff::SignedDuration::from_secs(30);
    enforce_ceiling_agents_with(
        &next_snap,
        &runtime,
        &RoomCapacities::default(),
        &config,
        || now + jiff::SignedDuration::from_secs(30),
        |req| {
            fired.push(req.clone());
            true
        },
    );
    assert_eq!(fired.len(), 2, "new compaction epoch bypasses prior pacing");
}

/// One crossing gets one compaction: a reading past both the smart-compaction line and a
/// RimZ-enforced level arms no reminder, so the hard compaction the ceiling fires is not followed
/// by a reminder to compact again; one armed below the level and not yet claimed is taken back
/// once the reading reaches it.
#[test]
fn a_reading_past_the_line_and_the_level_together_gets_the_ceiling_alone() {
    let now = ts(10_000);
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let line = AutoCompact::Tokens(258_000);
    let config = line_config(line);
    let pass = |seat: &AgentState| {
        let mut snap = snapshot(seat);
        snap.now = now;
        compact_agents(
            &snap,
            &runtime,
            &RoomCapacities::default(),
            &config,
            &SeatPolicy::default(),
            &DiagSink::disabled(),
        );
        let mut fired = 0;
        enforce_ceiling_agents_with(
            &snap,
            &runtime,
            &RoomCapacities::default(),
            &config,
            || now,
            |_| {
                fired += 1;
                true
            },
        );
        fired
    };
    let claimed = |seat: &AgentState| {
        claim_pending(&runtime, &seat.kind, None, Some(seat.agent_id.as_str())).is_some()
    };
    let mut candidate = agent();
    candidate.kind = AgentKind::new_unchecked("antigravity");
    candidate.record = Some(Box::new(crate::agents::LaunchRecord {
        auto_compact: Some(272_000),
        ..Default::default()
    }));

    candidate.usage.total_tokens = Some(280_000);
    assert_eq!(pass(&candidate), 1, "the ceiling fires once");
    assert_eq!(verdict(&candidate, line), ReminderVerdict::NotDue);
    assert!(!claimed(&candidate), "no reminder rides on the crossing");

    let mut later = candidate.clone();
    later.agent_id = "later".into();
    later.usage.total_tokens = Some(260_000);
    assert_eq!(pass(&later), 0);
    later.usage.total_tokens = Some(280_000);
    assert_eq!(pass(&later), 1);
    assert!(
        !claimed(&later),
        "a reminder armed below the level is taken back once the reading reaches it"
    );

    candidate.kind = AgentKind::new_unchecked("claude");
    assert!(
        verdict(&candidate, line).is_due(),
        "a native window leaves the reminder as it was"
    );
}
