use super::*;
use crate::agents::account::RateLimitsCache;
use crate::agents::{
    AgentContext, AgentRateLimits, AgentTurnError, ProviderCapacity, RateLimitWindow,
    TurnErrorClass,
};
use crate::ids::{AgentSessionId, MuxName, WorkspaceId};

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).expect("valid test timestamp")
}

fn rate_record(deadline: i64, activity: i64, last_nudge: Option<i64>, retries: u32) -> ParkRecord {
    ParkRecord {
        kind: ParkKind::RateLimit {
            deadline: ts(deadline),
        },
        parked_at_activity: ts(activity),
        last_nudge_at: last_nudge.map(ts),
        retries,
    }
}

fn overloaded_record(
    overloaded_at: i64,
    activity: i64,
    last_nudge: Option<i64>,
    retries: u32,
) -> ParkRecord {
    ParkRecord {
        kind: ParkKind::Overloaded {
            overloaded_at: ts(overloaded_at),
        },
        parked_at_activity: ts(activity),
        last_nudge_at: last_nudge.map(ts),
        retries,
    }
}

fn due(
    record: &ParkRecord,
    attempts: u32,
    now: i64,
    backoff_secs: &[u64],
    max_retries: u32,
) -> bool {
    nudge_due(record, attempts, ts(now), backoff_secs, max_retries)
}

fn resume_message(id: u64, status: MessageStatus, enqueued_at: i64) -> ResumeMessage {
    ResumeMessage {
        message_id: MessageId::parse(&format!("msg_{id:016x}")).expect("message id"),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "sess".into(),
        agent_name: None,
        status,
        enqueued_at: ts(enqueued_at),
        updated_at: ts(enqueued_at),
    }
}

fn window(used: u8, reset: i64) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(ts(reset)),
        duration_mins: Some(300),
        ..Default::default()
    }
}

fn temp_runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    (dir, runtime)
}

fn write_rate_limits_cache(runtime: &RuntimePaths, cache: &RateLimitsCache) {
    crate::store::atomic::write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), cache)
        .expect("write rate-limits cache");
}

fn write_recovered_window(runtime: &RuntimePaths) {
    write_rate_limits_cache(
        runtime,
        &RateLimitsCache {
            refreshed_at_ms: 0,
            entries: [(
                "claude".to_owned(),
                crate::agents::account::RateLimitCacheEntry {
                    limits: AgentRateLimits {
                        windows: vec![window(20, 9_000)],
                    },
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
}

#[test]
fn exact_qwen_cache_does_not_arm_session_resume_controls() {
    let (_dir, runtime) = temp_runtime();
    write_rate_limits_cache(
        &runtime,
        &RateLimitsCache {
            entries: [(
                "qwen".to_owned(),
                crate::agents::RateLimitCacheEntry {
                    scope: crate::agents::ProviderAccountScope::sub_provider(
                        "alibaba",
                        "international",
                    ),
                    account_key: Some("opaque-fingerprint".to_owned()),
                    limits: AgentRateLimits {
                        windows: vec![window(20, 9_000)],
                    },
                    pending: Vec::new(),
                    unknown_since_ms: None,
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    assert!(ProviderCapacity::read(&runtime, "qwen").is_none());
    assert!(ProviderCapacity::read_all(&runtime).is_empty());
}

fn park_path(runtime: &RuntimePaths) -> PathBuf {
    park_record_path(runtime, &AgentKind::new_unchecked("claude"), &"sess".into())
}

fn agent(activity: i64) -> AgentState {
    let mut agent = crate::sidebar::test_support::root_agent("claude", "sess", None);
    agent.name = None;
    agent.kind_ordinal = None;
    agent.last_seen = ts(activity);
    agent.last_activity = ts(activity);
    agent.registered_at = Some(ts(activity));
    agent
}

fn parked_agent(activity: i64, error_at: i64, class: TurnErrorClass, label: &str) -> AgentState {
    let mut agent = agent(activity);
    agent.context = Some(AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: Some(AgentTurnError {
            class,
            at: ts(error_at),
            label: Some(label.to_owned()),
        }),
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at: ts(error_at),
    });
    agent
}

#[test]
fn provider_limit_parks_arm_from_capacity_and_overload_arms_without_it() {
    let capacity = ProviderCapacity::from_windows(vec![
        window(100, 5_000),
        RateLimitWindow {
            duration_mins: Some(7 * 24 * 60),
            ..window(100, 9_000)
        },
    ]);
    for class in [
        TurnErrorClass::PausedRateLimit,
        TurnErrorClass::PausedSpendLimit,
    ] {
        assert_eq!(
            resume_park(
                &parked_agent(1_000, 1_010, class, "provider limit"),
                Some(&capacity),
                ts(2_000),
            ),
            Some(ResumeArm::RateLimit {
                deadline: ts(9_000)
            })
        );
    }
    assert_eq!(
        resume_park(
            &parked_agent(1_000, 1_010, TurnErrorClass::PausedOverloaded, "overloaded",),
            None,
            ts(2_000),
        ),
        Some(ResumeArm::Overloaded {
            overloaded_at: ts(1_010)
        })
    );
}

#[test]
fn provider_limit_does_not_arm_without_spent_future_reset() {
    let agent = parked_agent(
        1_000,
        1_010,
        TurnErrorClass::PausedRateLimit,
        "provider limit",
    );
    assert_eq!(resume_park(&agent, None, ts(2_000)), None);
    for capacity in [
        ProviderCapacity::from_windows(vec![window(99, 5_000)]),
        ProviderCapacity::from_windows(vec![window(100, 1_500)]),
        ProviderCapacity::default(),
    ] {
        assert_eq!(resume_park(&agent, Some(&capacity), ts(2_000)), None);
    }
}

fn live_pane() -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked("claude"),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some("sess".into()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    }
}

#[test]
fn arm_park_carries_or_resets_retry_state() {
    for (name, prior, kind, activity, expected) in [
        (
            "rate limit arms with deadline",
            None,
            ParkKind::RateLimit {
                deadline: ts(5_000),
            },
            1_000,
            rate_record(5_000, 1_000, None, 0),
        ),
        (
            "overloaded arms with activity",
            None,
            ParkKind::Overloaded {
                overloaded_at: ts(1_500),
            },
            1_000,
            overloaded_record(1_500, 1_000, None, 0),
        ),
        (
            "steady park keeps retry state",
            Some(overloaded_record(1_500, 1_000, Some(4_000), 3)),
            ParkKind::Overloaded {
                overloaded_at: ts(1_500),
            },
            1_000,
            overloaded_record(1_500, 1_000, Some(4_000), 3),
        ),
        (
            "regressed activity keeps baseline",
            Some(rate_record(5_000, 1_000, Some(5_000), 3)),
            ParkKind::RateLimit {
                deadline: ts(6_000),
            },
            900,
            rate_record(6_000, 1_000, Some(5_000), 3),
        ),
        (
            "new activity resets retry state",
            Some(overloaded_record(1_500, 1_000, Some(4_000), 3)),
            ParkKind::RateLimit {
                deadline: ts(9_000),
            },
            8_000,
            rate_record(9_000, 8_000, None, 0),
        ),
        (
            "new class resets retry state",
            Some(rate_record(5_000, 1_000, Some(5_000), 4)),
            ParkKind::Overloaded {
                overloaded_at: ts(1_500),
            },
            1_000,
            overloaded_record(1_500, 1_000, None, 0),
        ),
    ] {
        let (_dir, runtime) = temp_runtime();
        let path = park_path(&runtime);
        if let Some(prior) = prior {
            write_park(&path, &prior);
        }
        arm_park(&path, kind, ts(activity));
        assert_eq!(read_park(&path), Some(expected), "{name}");
    }
}

#[test]
fn still_parked_tracks_frozen_activity() {
    let record = rate_record(5_000, 1_000, None, 0);
    assert!(still_parked(&record, ts(900)));
    assert!(still_parked(&record, ts(1_000)));
    assert!(!still_parked(&record, ts(1_200)));
}

#[test]
fn only_day_budget_parks_arm_a_resume_deadline() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    let mut budgeted = agent(1_000);
    budgeted.budget_park = Some(crate::harness::budget::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::harness::budget::BudgetWindow::Day,
        at: ts(1_000),
        scope: crate::harness::budget::BudgetScope::Agent,
        account_kind: None,
        resets_at: Some(ts(5_000)),
    });
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![budgeted.clone()],
        ts(4_000),
    );
    resume_parked(
        &snapshot,
        &runtime,
        &ResumeConfig {
            auto_continue: true,
            ..ResumeConfig::default()
        },
        &[],
    );
    assert!(read_park(&path).is_some_and(|record| {
        matches!(record.kind, ParkKind::Budget { deadline } if deadline == ts(5_000))
    }));

    remove_park(&path);
    budgeted.budget_park = Some(crate::harness::budget::BudgetPark {
        window: crate::harness::budget::BudgetWindow::Session,
        resets_at: None,
        ..budgeted.budget_park.expect("day park")
    });
    let snapshot =
        SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), vec![budgeted], ts(4_000));
    resume_parked(
        &snapshot,
        &runtime,
        &ResumeConfig {
            auto_continue: true,
            ..ResumeConfig::default()
        },
        &[],
    );
    assert!(read_park(&path).is_none());
}

#[test]
fn fire_if_due_keeps_records_when_activity_regresses() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    let record = rate_record(5_000, 1_000, None, 0);
    write_park(&path, &record);
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![agent(900)],
        ts(4_000),
    );
    let config = ResumeConfig::default();
    fire_if_due(
        &snapshot.agents[0],
        &path,
        FireContext {
            snapshot: &snapshot,
            runtime: &runtime,
            now: ts(4_000),
            text: "continue",
            config: &config,
            resume_messages: &[],
        },
    );
    assert_eq!(read_park(&path), Some(record));
}

#[test]
fn nudge_due_truth_table() {
    for (name, record, attempts, now, backoff, max_retries, expected) in [
        (
            "rate limit waits before deadline",
            rate_record(5_000, 1_000, None, 0),
            0,
            4_999,
            &[][..],
            10,
            false,
        ),
        (
            "rate limit fires at deadline",
            rate_record(5_000, 1_000, None, 0),
            0,
            5_000,
            &[][..],
            10,
            true,
        ),
        (
            "rate limit recent nudge throttles",
            rate_record(5_000, 1_000, Some(5_000), 1),
            1,
            5_060,
            &[][..],
            10,
            false,
        ),
        (
            "rate limit retry interval elapses",
            rate_record(5_000, 1_000, Some(5_000), 1),
            1,
            5_200,
            &[][..],
            10,
            true,
        ),
        (
            "rate limit cap stops nudges",
            rate_record(5_000, 1_000, Some(5_000), 0),
            3,
            9_000,
            &[][..],
            3,
            false,
        ),
        (
            "rate limit under cap can retry",
            rate_record(5_000, 1_000, Some(5_000), 99),
            2,
            5_200,
            &[][..],
            3,
            true,
        ),
        (
            "first overload waits from park time",
            overloaded_record(1_000, 100, None, 0),
            0,
            1_059,
            &[60, 120, 180],
            10,
            false,
        ),
        (
            "first overload fires after backoff",
            overloaded_record(1_000, 100, None, 0),
            0,
            1_060,
            &[60, 120, 180],
            10,
            true,
        ),
        (
            "second overload waits on backoff",
            overloaded_record(1_000, 100, Some(1_060), 1),
            1,
            1_179,
            &[60, 120, 180],
            10,
            false,
        ),
        (
            "second overload fires after backoff",
            overloaded_record(1_000, 100, Some(1_060), 1),
            1,
            1_180,
            &[60, 120, 180],
            10,
            true,
        ),
        (
            "later overload waits on last backoff",
            overloaded_record(1_000, 100, Some(1_180), 2),
            2,
            1_359,
            &[60, 120, 180],
            10,
            false,
        ),
        (
            "later overload fires after last backoff",
            overloaded_record(1_000, 100, Some(1_180), 2),
            2,
            1_360,
            &[60, 120, 180],
            10,
            true,
        ),
        (
            "overload cap stops nudges",
            overloaded_record(1_000, 100, Some(1_000), 0),
            10,
            9_000,
            &[60, 120, 180],
            10,
            false,
        ),
        (
            "overload under cap can retry",
            overloaded_record(1_000, 100, Some(1_000), 9),
            9,
            1_180,
            &[60, 120, 180],
            10,
            true,
        ),
        (
            "evidenced attempt cap stops nudges",
            rate_record(5_000, 1_000, Some(5_000), 0),
            3,
            6_000,
            &[][..],
            3,
            false,
        ),
    ] {
        assert_eq!(
            due(&record, attempts, now, backoff, max_retries),
            expected,
            "{name}"
        );
    }
}

#[test]
fn overload_backoff_expands_then_repeats_the_last_step() {
    assert_eq!(overload_backoff(0, &[60, 120, 180]).as_secs(), 60);
    assert_eq!(overload_backoff(1, &[60, 120, 180]).as_secs(), 120);
    assert_eq!(overload_backoff(2, &[60, 120, 180]).as_secs(), 180);
    assert_eq!(overload_backoff(9, &[60, 120, 180]).as_secs(), 180);
    assert_eq!(overload_backoff(0, &[]).as_secs(), 300);
}

#[test]
fn stalled_stream_park_uses_default_three_minute_retry() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    let label = "API Error: Response stalled mid-stream. The response above may be incomplete.";
    let config = ResumeConfig {
        auto_continue: true,
        ..ResumeConfig::default()
    };
    assert_eq!(config.auto_continue_text, "continue");
    let snapshot_at = |now| {
        let mut snapshot = SidebarSnapshot::build_with_agents(
            runtime.workspace_id.clone(),
            vec![parked_agent(
                100,
                1_000,
                TurnErrorClass::PausedOverloaded,
                label,
            )],
            ts(now),
        );
        snapshot.now = ts(now);
        snapshot.agent_panes = vec![live_pane()];
        snapshot
    };

    resume_parked(&snapshot_at(1_179), &runtime, &config, &[]);
    assert_eq!(
        read_park(&path),
        Some(overloaded_record(1_000, 100, None, 0))
    );

    resume_parked(&snapshot_at(1_180), &runtime, &config, &[]);
    assert_eq!(
        read_park(&path),
        Some(overloaded_record(1_000, 100, Some(1_180), 1))
    );
}

#[test]
fn recovered_budget_fires_due_rate_limit_record_before_clearing() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, None, 0));
    write_recovered_window(&runtime);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![parked_agent(
            1_000,
            5_990,
            TurnErrorClass::PausedRateLimit,
            "You've hit your usage limit",
        )],
        ts(6_000),
    );
    snapshot.now = ts(6_000);
    snapshot.agent_panes = vec![live_pane()];
    resume_parked(
        &snapshot,
        &runtime,
        &ResumeConfig {
            auto_continue: true,
            auto_continue_max_retries: 3,
            ..ResumeConfig::default()
        },
        &[],
    );
    assert_eq!(
        read_park(&path),
        Some(rate_record(5_000, 1_000, Some(6_000), 1))
    );
}

#[test]
fn recovered_budget_rearms_a_lost_limit_park() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_recovered_window(&runtime);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![parked_agent(
            1_000,
            5_990,
            TurnErrorClass::PausedRateLimit,
            "You've hit your usage limit",
        )],
        ts(6_000),
    );
    snapshot.now = ts(6_000);
    snapshot.agent_panes = vec![live_pane()];
    resume_parked(
        &snapshot,
        &runtime,
        &ResumeConfig {
            auto_continue: true,
            auto_continue_max_retries: 3,
            ..ResumeConfig::default()
        },
        &[],
    );
    assert_eq!(
        read_park(&path),
        Some(rate_record(6_000, 1_000, Some(6_000), 1))
    );
}

#[test]
fn recovered_budget_clears_a_stale_rate_limit_record() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 1));
    write_recovered_window(&runtime);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![agent(1_000)],
        ts(6_000),
    );
    snapshot.now = ts(6_000);
    resume_parked(
        &snapshot,
        &runtime,
        &ResumeConfig {
            auto_continue: true,
            ..ResumeConfig::default()
        },
        &[],
    );
    assert_eq!(read_park(&path), None);
}

#[test]
fn nudging_records_the_time_and_increments_retries() {
    let nudged = nudged_record(overloaded_record(1_000, 100, None, 2), ts(1_060));
    assert_eq!(nudged.last_nudge_at, Some(ts(1_060)));
    assert_eq!(nudged.retries, 3);
}

fn fire_with_resume_message(status: MessageStatus) -> Option<ParkRecord> {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 1));
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![parked_agent(
            1_000,
            5_990,
            TurnErrorClass::PausedRateLimit,
            "You've hit your usage limit",
        )],
        ts(6_000),
    );
    snapshot.now = ts(6_000);
    snapshot.agent_panes = vec![live_pane()];
    let config = ResumeConfig {
        auto_continue_max_retries: 3,
        ..ResumeConfig::default()
    };
    let messages = [resume_message(1, status, 5_900)];
    fire_if_due(
        &snapshot.agents[0],
        &path,
        FireContext {
            snapshot: &snapshot,
            runtime: &runtime,
            now: ts(6_000),
            text: "continue",
            config: &config,
            resume_messages: &messages,
        },
    );
    read_park(&path)
}

#[test]
fn undelivered_resume_messages_allow_retry_under_cap() {
    for status in [
        MessageStatus::Sent,
        MessageStatus::Queued,
        MessageStatus::Abandoned,
    ] {
        assert_eq!(
            fire_with_resume_message(status),
            Some(rate_record(5_000, 1_000, Some(6_000), 2)),
            "{status:?}"
        );
    }
}

#[test]
fn duplicate_resume_messages_count_as_one_attempt() {
    let agent = parked_agent(
        1_000,
        5_990,
        TurnErrorClass::PausedRateLimit,
        "You've hit your usage limit",
    );
    let record = rate_record(5_000, 1_000, Some(5_000), 0);
    let duplicate = ResumeMessage {
        status: MessageStatus::TimedOut,
        updated_at: ts(6_100),
        ..resume_message(1, MessageStatus::Queued, 5_900)
    };
    let messages = [
        resume_message(1, MessageStatus::Queued, 5_900),
        duplicate,
        resume_message(2, MessageStatus::Queued, 900),
    ];

    assert_eq!(evidenced_attempts(&messages, &agent, &record), 1);
}

#[test]
fn delivered_resume_message_clears_the_park() {
    assert_eq!(fire_with_resume_message(MessageStatus::Delivered), None);
}

#[test]
fn phantom_spawns_never_exhaust_a_park() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    let record = rate_record(5_000, 1_000, Some(5_000), 29);
    write_park(&path, &record);
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![parked_agent(
            1_000,
            5_990,
            TurnErrorClass::PausedRateLimit,
            "You've hit your usage limit",
        )],
        ts(6_000),
    );
    let config = ResumeConfig {
        auto_continue: true,
        auto_continue_max_retries: 13,
        ..ResumeConfig::default()
    };

    assert!(nudge_due(&record, 0, ts(6_000), &[], 13));
    assert!(exhausted_parks(&snapshot, &runtime, &config, &[]).is_empty());
}

#[test]
fn exhausted_resume_attempts_report_actionable_key() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 0));
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        vec![parked_agent(
            1_000,
            5_990,
            TurnErrorClass::PausedRateLimit,
            "You've hit your usage limit",
        )],
        ts(6_000),
    );
    let config = ResumeConfig {
        auto_continue: true,
        auto_continue_max_retries: 3,
        ..ResumeConfig::default()
    };
    let messages = [
        resume_message(1, MessageStatus::TimedOut, 5_900),
        resume_message(2, MessageStatus::Errored, 5_920),
        resume_message(3, MessageStatus::Queued, 5_940),
    ];
    assert!(
        exhausted_parks(&snapshot, &runtime, &config, &messages).contains(&(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("sess")
        ))
    );
}
