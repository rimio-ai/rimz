use std::time::Duration;

use jiff::Timestamp;

use super::*;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, MessageId, WorkspaceId};

#[test]
fn gates_open_only_on_resting_statuses() {
    assert!(gate_open(DeliveryGate::Done, AgentStatus::Idle));
    assert!(gate_open(DeliveryGate::Done, AgentStatus::Success));
    assert!(!gate_open(DeliveryGate::Done, AgentStatus::Failed));
    assert!(gate_open(DeliveryGate::Any, AgentStatus::Failed));
    for status in [
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Paused,
    ] {
        assert!(!gate_open(DeliveryGate::Done, status));
        assert!(!gate_open(DeliveryGate::Any, status));
    }
}

#[test]
fn delivery_checkpoint_is_only_unparked_turn_end() {
    assert!(delivery_checkpoint(&LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
    }));
    assert!(delivery_checkpoint(&LifecycleSignal::TurnEnded {
        errored: true,
        parked_on_background: false,
    }));
    assert!(!delivery_checkpoint(&LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: true,
    }));
    assert!(!delivery_checkpoint(&LifecycleSignal::Registered));
    assert!(!delivery_checkpoint(&LifecycleSignal::SubagentStopped {
        errored: false
    }));
}

#[test]
fn message_status_lifecycle_helpers_match_queue_semantics() {
    assert!(MessageStatus::Queued.is_open());
    assert!(MessageStatus::Claimed.is_open());
    assert!(!MessageStatus::Sent.is_open());
    assert!(!MessageStatus::Sent.is_terminal());
    for status in [
        MessageStatus::Delivered,
        MessageStatus::TimedOut,
        MessageStatus::Errored,
        MessageStatus::Removed,
        MessageStatus::Abandoned,
        MessageStatus::Archived,
    ] {
        assert!(status.is_terminal(), "{status}");
    }
    let legacy: MessageStatus = serde_json::from_str("\"pending\"").unwrap();
    assert_eq!(legacy, MessageStatus::Queued);
}

#[test]
fn claim_ttl_treats_future_stamp_as_expired() {
    let now = Timestamp::now();
    assert!(claim_expired(None, now));
    assert!(!claim_expired(
        Some(now - jiff::SignedDuration::from_secs(1)),
        now
    ));
    assert!(claim_expired(
        Some(now - jiff::SignedDuration::from_secs(15)),
        now
    ));
    assert!(claim_expired(
        Some(now + jiff::SignedDuration::from_secs(60)),
        now
    ));
}

#[test]
fn message_matches_registered_card_by_remembered_name() {
    let mut provisional = agent("launch_1", Some("lucid-atlas"));
    let message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &provisional,
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    provisional.agent_id = AgentSessionId::from("real-session");

    assert!(message.same_agent_card(&provisional));
}

#[test]
fn auto_compact_parses_percent_and_token_forms() {
    assert_eq!(AutoCompact::parse("70%").unwrap(), AutoCompact::Percent(70));
    assert_eq!(AutoCompact::parse(" 0% ").unwrap(), AutoCompact::Percent(0));
    assert_eq!(
        AutoCompact::parse("120000").unwrap(),
        AutoCompact::Tokens(120_000)
    );
    assert!(AutoCompact::parse("101%").is_err());
    assert!(AutoCompact::parse("abc").is_err());
    assert!(AutoCompact::parse("70.5%").is_err());
}

#[test]
fn auto_compact_triggers_only_once_fill_is_reached() {
    let mut a = agent("s1", None);
    // An unknown fill is not a full window.
    assert!(!AutoCompact::Percent(70).triggered(&a));
    assert!(!AutoCompact::Tokens(1).triggered(&a));
    // The percent threshold reads the carried gauge.
    a.context_pct = Some(75);
    assert!(AutoCompact::Percent(70).triggered(&a));
    assert!(AutoCompact::Percent(75).triggered(&a));
    assert!(!AutoCompact::Percent(76).triggered(&a));
    // The token threshold reads the per-call split fallback.
    a.cache_read_input_tokens = Some(100_000);
    a.fresh_input_tokens = Some(20_000);
    assert!(AutoCompact::Tokens(120_000).triggered(&a));
    assert!(!AutoCompact::Tokens(120_001).triggered(&a));
}

#[test]
fn auto_compact_round_trips_through_a_message_record() {
    let message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_auto_compact(Some(AutoCompact::Percent(70)));
    let json = serde_json::to_string(&message).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.auto_compact, Some(AutoCompact::Percent(70)));
}

#[test]
fn not_before_defaults_ready_and_round_trips_when_set() {
    let base = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let now = Timestamp::now();
    assert_eq!(base.not_before, None);
    assert!(base.is_ready(now));

    let scheduled_at = now + jiff::SignedDuration::from_secs(60);
    let scheduled = base.with_not_before(Some(scheduled_at));
    assert!(!scheduled.is_ready(now));
    assert!(scheduled.is_ready(scheduled_at));
    let json = serde_json::to_string(&scheduled).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.not_before, Some(scheduled_at));
    let mut legacy = serde_json::to_value(&back).unwrap();
    legacy.as_object_mut().unwrap().remove("not_before");
    let back: MessageRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(back.not_before, None);
}

#[test]
fn retry_after_defaults_absent_and_round_trips_when_set() {
    let mut message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let retry_at = Timestamp::now() + jiff::SignedDuration::from_secs(30);
    assert_eq!(message.retry_after, None);

    message.retry_after = Some(retry_at);
    let json = serde_json::to_string(&message).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.retry_after, Some(retry_at));

    let mut legacy = serde_json::to_value(&back).unwrap();
    legacy.as_object_mut().unwrap().remove("retry_after");
    let back: MessageRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(back.retry_after, None);
}

#[test]
fn sent_reconcile_deadline_uses_updated_at_only_for_sent_records() {
    let mut message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let updated_at = Timestamp::from_second(1_000).unwrap();
    message.updated_at = updated_at;

    assert_eq!(
        message.sent_reconcile_deadline(Duration::from_secs(30)),
        None
    );

    message.status = MessageStatus::Sent;
    assert_eq!(
        message.sent_reconcile_deadline(Duration::from_secs(30)),
        Some(updated_at + Duration::from_secs(30))
    );
}

#[test]
fn wake_deadline_arms_queued_and_sent_records() {
    let base = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let now = Timestamp::from_second(1_000).unwrap();
    let updated_at = now - jiff::SignedDuration::from_secs(5);
    let retry_after = now + jiff::SignedDuration::from_secs(30);
    let window = Duration::from_secs(30);
    let mut message = base.clone();
    message.updated_at = updated_at;
    let cases = [
        (message.clone(), Some(updated_at)),
        (
            MessageRecord {
                retry_after: Some(retry_after),
                ..message.clone()
            },
            Some(retry_after),
        ),
        (
            MessageRecord {
                retry_after: Some(retry_after),
                ..message
                    .clone()
                    .with_not_before(Some(now + jiff::SignedDuration::from_secs(60)))
            },
            Some(now + jiff::SignedDuration::from_secs(60)),
        ),
        (
            MessageRecord {
                retry_after: Some(retry_after),
                not_before: Some(now - jiff::SignedDuration::from_secs(60)),
                ..message.clone()
            },
            Some(retry_after),
        ),
        (
            MessageRecord {
                status: MessageStatus::Sent,
                ..message.clone()
            },
            Some(updated_at + window),
        ),
        (message.with_status(MessageStatus::Delivered), None),
    ];
    for (message, expected) in cases {
        assert_eq!(message.wake_deadline(now, window), expected);
    }
}

#[test]
fn message_body_defaults_to_prompt_and_command_round_trips() {
    let base = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert_eq!(base.body, MessageBody::Prompt);
    let command = base.with_body(MessageBody::Command);
    let json = serde_json::to_string(&command).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.body, MessageBody::Command);
    let mut legacy = serde_json::to_value(&back).unwrap();
    legacy.as_object_mut().unwrap().remove("body");
    let back: MessageRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(back.body, MessageBody::Prompt);
}

#[test]
fn force_defaults_off_and_round_trips_when_set() {
    let base = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert!(
        !base.force,
        "a fresh record never forces past a pending ask"
    );
    let forced = base.with_force(true);
    let json = serde_json::to_string(&forced).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert!(back.force);
    // A record written before the field existed reads as not-forced.
    let legacy = json.replace(",\"force\":true", "");
    let back: MessageRecord = serde_json::from_str(&legacy).unwrap();
    assert!(!back.force);
}

#[test]
fn sender_defaults_to_human_and_agent_round_trips() {
    let base = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert_eq!(base.sender, MessageSender::Human);
    let agent_sender = MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("lucid-atlas".to_owned()),
        profile: Some("planner".to_owned()),
        role: None,
        channel: Some("main".to_owned()),
    };
    let attributed = base.with_sender(agent_sender.clone());
    let json = serde_json::to_string(&attributed).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.sender, agent_sender);
    // A record written before sender attribution reads as human-authored.
    let mut legacy = serde_json::to_value(&attributed).unwrap();
    legacy.as_object_mut().unwrap().remove("sender");
    let back: MessageRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(back.sender, MessageSender::Human);
}

#[test]
fn channel_defaults_absent_and_round_trips_when_set() {
    let base = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("s1", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert_eq!(base.channel, None);
    let scoped = base.with_channel(Some("docs".to_owned()));
    let json = serde_json::to_string(&scoped).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.channel.as_deref(), Some("docs"));
    let mut legacy = serde_json::to_value(&back).unwrap();
    legacy.as_object_mut().unwrap().remove("channel");
    let back: MessageRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(back.channel, None);
}

#[test]
fn sender_render_names_human_and_agent_address() {
    assert_eq!(MessageSender::Human.render(), "you");
    assert_eq!(
        MessageSender::Agent {
            kind: AgentKind::new_unchecked("claude"),
            name: Some("lucid-atlas".to_owned()),
            profile: Some("planner".to_owned()),
            role: None,
            channel: Some("docs".to_owned()),
        }
        .render(),
        "@lucid-atlas#docs"
    );
    assert_eq!(
        MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: None,
            channel: None,
        }
        .render(),
        "@codex"
    );
}

#[test]
fn auto_compact_tokens_threshold_reads_the_carried_total() {
    // A transcript-derived session reports only a running total — no rich
    // context blob and no per-call split. The percent gauge already scales
    // off that total, so the token threshold must read it too rather than
    // silently never firing.
    let mut a = agent("s1", None);
    a.total_tokens = Some(120_000);
    a.context_window = Some(200_000);
    assert!(AutoCompact::Tokens(100_000).triggered(&a));
    assert!(AutoCompact::Tokens(120_000).triggered(&a));
    assert!(!AutoCompact::Tokens(120_001).triggered(&a));
}

#[test]
fn queue_head_spans_provisional_and_registered_ids() {
    // A message queued against a provisional `launch_*` card and a later
    // message queued after the card registers share one logical agent, so
    // FIFO must return the older provisional-card message as the head.
    let provisional = agent("launch_1", Some("lucid-atlas"));
    let registered = agent("real-session", Some("lucid-atlas"));
    let ws = WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message"));
    let mut older = MessageRecord::new(
        ws.clone(),
        &provisional,
        "first".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let mut newer = MessageRecord::new(
        ws,
        &registered,
        "second".to_owned(),
        true,
        DeliveryGate::Done,
    );
    older.message_id = MessageId::parse("msg_0000000000000001").unwrap();
    newer.message_id = MessageId::parse("msg_0000000000000002").unwrap();
    let pending = [newer.clone(), older.clone()];

    let head = queue_head(
        pending.iter(),
        &registered.kind,
        &registered.agent_id,
        registered.name.as_deref(),
        Timestamp::now(),
    )
    .expect("the registered observation selects a head");
    assert_eq!(
        head.message_id, older.message_id,
        "the older provisional-card message is the head, not the newer registered one"
    );

    // Without the stable name the provisional record is invisible to the
    // registered id — the reordering this fix closes.
    let exact = queue_head(
        pending.iter(),
        &registered.kind,
        &registered.agent_id,
        None,
        Timestamp::now(),
    )
    .expect("the registered id still matches its own record");
    assert_eq!(exact.message_id, newer.message_id);
}

#[test]
fn queue_head_skips_not_yet_ready_scheduled_messages() {
    let agent = agent("real-session", Some("lucid-atlas"));
    let ws = WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message"));
    let now = Timestamp::now();
    let mut scheduled = MessageRecord::new(
        ws.clone(),
        &agent,
        "later".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_not_before(Some(now + jiff::SignedDuration::from_secs(60)));
    let mut ready = MessageRecord::new(ws, &agent, "now".to_owned(), true, DeliveryGate::Done);
    scheduled.message_id = MessageId::parse("msg_0000000000000001").unwrap();
    ready.message_id = MessageId::parse("msg_0000000000000002").unwrap();
    let pending = [scheduled.clone(), ready.clone()];

    let head = queue_head(
        pending.iter(),
        &agent.kind,
        &agent.agent_id,
        agent.name.as_deref(),
        now,
    )
    .expect("ready message is selected");

    assert_eq!(head.message_id, ready.message_id);
}

#[test]
fn queue_head_does_not_treat_retry_after_as_readiness() {
    let agent = agent("real-session", Some("lucid-atlas"));
    let ws = WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message"));
    let now = Timestamp::now();
    let mut deferred = MessageRecord::new(
        ws.clone(),
        &agent,
        "old".to_owned(),
        true,
        DeliveryGate::Done,
    );
    deferred.retry_after = Some(now + jiff::SignedDuration::from_secs(60));
    let mut newer = MessageRecord::new(ws, &agent, "new".to_owned(), true, DeliveryGate::Done);
    deferred.message_id = MessageId::parse("msg_0000000000000001").unwrap();
    newer.message_id = MessageId::parse("msg_0000000000000002").unwrap();
    let pending = [deferred.clone(), newer];

    let head = queue_head(
        pending.iter(),
        &agent.kind,
        &agent.agent_id,
        agent.name.as_deref(),
        now,
    )
    .expect("retry_after does not hide the FIFO head");

    assert_eq!(head.message_id, deferred.message_id);
}

#[test]
fn parse_schedule_at_accepts_duration_and_next_wall_clock_time() {
    let now = jiff::civil::date(2026, 6, 24)
        .at(8, 0, 0, 0)
        .in_tz("UTC")
        .unwrap();
    assert_eq!(
        parse_schedule_at("90s", &now).unwrap(),
        now.timestamp() + jiff::SignedDuration::from_secs(90)
    );
    assert_eq!(
        parse_schedule_at("08:30", &now).unwrap(),
        jiff::civil::date(2026, 6, 24)
            .at(8, 30, 0, 0)
            .in_tz("UTC")
            .unwrap()
            .timestamp()
    );
    assert_eq!(
        parse_schedule_at("07:30", &now).unwrap(),
        jiff::civil::date(2026, 6, 25)
            .at(7, 30, 0, 0)
            .in_tz("UTC")
            .unwrap()
            .timestamp()
    );
    assert!(parse_schedule_at("0s", &now).is_err());
    assert!(parse_schedule_at("tomorrow", &now).is_err());
    assert!(parse_schedule_at("25:00", &now).is_err());
}

fn agent(id: &str, name: Option<&str>) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        agent_id: AgentSessionId::from(id),
        kind: AgentKind::new_unchecked("claude"),
        name: name.map(ToOwned::to_owned),
        kind_ordinal: Some(1),
        profile: None,
        role: None,
        team: None,
        channel: None,
        status: AgentStatus::Idle,
        phase: crate::agents::TurnPhase::Idle,
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
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}
