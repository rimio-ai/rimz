use super::*;
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentContext, AgentRateLimits, AgentStatus, AgentTurnError, RateLimitWindow, TurnErrorClass,
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

fn park_path(runtime: &RuntimePaths) -> PathBuf {
    park_record_path(runtime, &AgentKind::new_unchecked("claude"), &"sess".into())
}

fn agent(activity: i64) -> AgentState {
    AgentState {
        agent_id: "sess".into(),
        kind: AgentKind::new_unchecked("claude"),
        name: None,
        kind_ordinal: None,
        profile: None,
        role: None,
        team: None,
        channel: None,
        status: AgentStatus::Running,
        phase: TurnPhase::Idle,
        pane: None,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_compact_command_tokens: None,
        last_seen: ts(activity),
        last_activity: ts(activity),
        registered_at: Some(ts(activity)),
    }
}

fn limit_agent(activity: i64, error_at: i64) -> AgentState {
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
        turn_error: Some(AgentTurnError {
            class: TurnErrorClass::PausedRateLimit,
            at: ts(error_at),
            label: Some("You've hit your usage limit".to_owned()),
        }),
        turn_complete: None,
        observed_at: ts(error_at),
    });
    agent
}

fn live_pane() -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked("claude"),
        kind_ordinal: None,
        name: None,
        profile: None,
        role: None,
        team: None,
        channel: None,
        agent_id: Some("sess".into()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        worktree_path: None,
        worktree_branch: None,
    }
}

#[test]
fn arms_a_rate_limit_park_with_its_deadline_and_activity() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    arm_park(
        &path,
        ParkKind::RateLimit {
            deadline: ts(5_000),
        },
        ts(1_000),
    );
    assert_eq!(read_park(&path), Some(rate_record(5_000, 1_000, None, 0)));
}

#[test]
fn arms_an_overloaded_park_with_its_activity() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    arm_park(
        &path,
        ParkKind::Overloaded {
            overloaded_at: ts(1_500),
        },
        ts(1_000),
    );
    assert_eq!(
        read_park(&path),
        Some(overloaded_record(1_500, 1_000, None, 0))
    );
}

#[test]
fn a_steady_park_keeps_its_nudge_stamp_and_retry_count() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &overloaded_record(1_500, 1_000, Some(4_000), 3));
    // Re-arm at the same activity (the agent is still idle): retry state survives.
    arm_park(
        &path,
        ParkKind::Overloaded {
            overloaded_at: ts(1_500),
        },
        ts(1_000),
    );
    assert_eq!(
        read_park(&path),
        Some(overloaded_record(1_500, 1_000, Some(4_000), 3))
    );
}

#[test]
fn a_regressed_park_keeps_its_baseline_and_retry_state() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 3));
    arm_park(
        &path,
        ParkKind::RateLimit {
            deadline: ts(6_000),
        },
        ts(900),
    );
    assert_eq!(
        read_park(&path),
        Some(rate_record(6_000, 1_000, Some(5_000), 3))
    );
}

#[test]
fn a_new_park_baseline_resets_the_throttle_and_retry_count() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &overloaded_record(1_500, 1_000, Some(4_000), 3));
    // The agent acted (activity advanced) and re-parked: a fresh nudge may fire.
    arm_park(
        &path,
        ParkKind::RateLimit {
            deadline: ts(9_000),
        },
        ts(8_000),
    );
    assert_eq!(read_park(&path), Some(rate_record(9_000, 8_000, None, 0)));
}

#[test]
fn a_new_park_class_resets_the_throttle_and_retry_count() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 4));
    arm_park(
        &path,
        ParkKind::Overloaded {
            overloaded_at: ts(1_500),
        },
        ts(1_000),
    );
    assert_eq!(
        read_park(&path),
        Some(overloaded_record(1_500, 1_000, None, 0))
    );
}

#[test]
fn still_parked_tracks_frozen_activity() {
    let record = rate_record(5_000, 1_000, None, 0);
    assert!(still_parked(&record, ts(900)));
    assert!(still_parked(&record, ts(1_000)));
    assert!(!still_parked(&record, ts(1_200)));
}

#[test]
fn fire_if_due_keeps_records_when_activity_regresses() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    let record = rate_record(5_000, 1_000, None, 0);
    write_park(&path, &record);
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
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
fn rate_limit_nudge_waits_for_the_deadline_then_fires() {
    let record = rate_record(5_000, 1_000, None, 0);
    assert!(!due(&record, 0, 4_999, &[], 10));
    assert!(due(&record, 0, 5_000, &[], 10));
}

#[test]
fn rate_limit_recent_nudge_throttles_the_next() {
    // Last nudge at 5_000; the retry interval is 120s.
    let record = rate_record(5_000, 1_000, Some(5_000), 1);
    assert!(!due(&record, 1, 5_060, &[], 10));
    assert!(due(&record, 1, 5_200, &[], 10));
}

#[test]
fn rate_limit_retry_cap_stops_further_nudges() {
    let at_cap = rate_record(5_000, 1_000, Some(5_000), 0);
    assert!(!due(&at_cap, 3, 9_000, &[], 3));

    let before_cap = rate_record(5_000, 1_000, Some(5_000), 99);
    assert!(due(&before_cap, 2, 5_200, &[], 3));
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
fn first_overloaded_nudge_waits_from_the_park_time() {
    let record = overloaded_record(1_000, 100, None, 0);
    assert!(!due(&record, 0, 1_059, &[60, 120, 180], 10));
    assert!(due(&record, 0, 1_060, &[60, 120, 180], 10));
}

#[test]
fn overloaded_retries_wait_on_their_backoff_step() {
    let second = overloaded_record(1_000, 100, Some(1_060), 1);
    assert!(!due(&second, 1, 1_179, &[60, 120, 180], 10));
    assert!(due(&second, 1, 1_180, &[60, 120, 180], 10));

    let later = overloaded_record(1_000, 100, Some(1_180), 2);
    assert!(!due(&later, 2, 1_359, &[60, 120, 180], 10));
    assert!(due(&later, 2, 1_360, &[60, 120, 180], 10));
}

#[test]
fn overloaded_retry_cap_stops_further_nudges() {
    let at_cap = overloaded_record(1_000, 100, Some(1_000), 0);
    assert!(!due(&at_cap, 10, 9_000, &[60, 120, 180], 10));

    let before_cap = overloaded_record(1_000, 100, Some(1_000), 9);
    assert!(due(&before_cap, 9, 1_180, &[60, 120, 180], 10));
}

#[test]
fn recovered_budget_fires_due_rate_limit_record_before_clearing() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, None, 0));
    super::super::rate_limits::write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &super::super::rate_limits::RateLimitsCache {
            refreshed_at_ms: 0,
            windows: [(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![window(20, 9_000)],
                },
            )]
            .into_iter()
            .collect(),
            pending: Default::default(),
        },
    );
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
        vec![limit_agent(1_000, 5_990)],
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
    super::super::rate_limits::write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &super::super::rate_limits::RateLimitsCache {
            refreshed_at_ms: 0,
            windows: [(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![window(20, 9_000)],
                },
            )]
            .into_iter()
            .collect(),
            pending: Default::default(),
        },
    );
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
        vec![limit_agent(1_000, 5_990)],
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
    super::super::rate_limits::write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &super::super::rate_limits::RateLimitsCache {
            refreshed_at_ms: 0,
            windows: [(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![window(20, 9_000)],
                },
            )]
            .into_iter()
            .collect(),
            pending: Default::default(),
        },
    );
    let mut snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
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
        Vec::new(),
        vec![limit_agent(1_000, 5_990)],
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
    let agent = limit_agent(1_000, 5_990);
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
        Vec::new(),
        vec![limit_agent(1_000, 5_990)],
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
fn evidenced_attempt_cap_stops_further_nudges() {
    let record = rate_record(5_000, 1_000, Some(5_000), 0);

    assert!(!nudge_due(&record, 3, ts(6_000), &[], 3));
}

#[test]
fn exhausted_resume_attempts_report_actionable_key() {
    let (_dir, runtime) = temp_runtime();
    let path = park_path(&runtime);
    write_park(&path, &rate_record(5_000, 1_000, Some(5_000), 0));
    let snapshot = SidebarSnapshot::build_with_agents(
        runtime.workspace_id.clone(),
        Vec::new(),
        vec![limit_agent(1_000, 5_990)],
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
