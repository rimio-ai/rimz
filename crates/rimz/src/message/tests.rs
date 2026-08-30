use std::time::Duration;

use jiff::Timestamp;

use super::*;
use crate::agents::{
    AgentContext, AgentState, AgentStatus, LifecycleSignal, TurnSettle, TurnSettleOutcome,
};
use crate::ids::{AgentKind, MessageId, MuxName, PaneId, WorkspaceId};
use crate::store::snapshot::PaneAgent;

#[test]
fn draft_record_uses_recipient_identity_and_live_pane_context() {
    let agent = agent("sess-a", Some("lucid-atlas"));
    let pane = pane(
        "claude",
        Some("sess-a"),
        Some("lucid-atlas"),
        Some("auth"),
        None,
        "terminal_1",
    );
    let workspace_id = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let not_before = Timestamp::now();

    let live = draft(Some(not_before)).record(
        workspace_id.clone(),
        Recipient::Agent {
            agent: &agent,
            pane: Some(&pane),
        },
        None,
        "hello",
        Some("@claude"),
    );
    let parked = draft(Some(not_before)).record(
        workspace_id,
        Recipient::Agent {
            agent: &agent,
            pane: None,
        },
        None,
        "hello",
        Some("@claude"),
    );

    assert_eq!(live.agent_id, agent.agent_id);
    assert_eq!(live.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(live.channel.as_deref(), Some("auth"));
    assert_eq!(live.not_before, Some(not_before));
    assert_eq!(parked.pane_id, None);
    assert_eq!(parked.channel, None);
}

#[test]
fn draft_record_normalizes_bound_provisional_lazy_and_pane_identity() {
    let workspace_id = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let mut bound = agent("sess-a", Some("lucid-atlas"));
    bound.channel = Some("bound-channel".to_owned());
    let bound_pane = pane(
        "claude",
        Some("pane-session"),
        Some("pane-name"),
        Some("pane-channel"),
        Some("/repo/pane-worktree"),
        "terminal_1",
    );
    let record = pane_record(workspace_id.clone(), &bound_pane, Some(&bound), "scope");
    assert_eq!(record.agent_id, bound.agent_id);
    assert_eq!(record.agent_name, bound.name);
    assert_eq!(record.channel.as_deref(), Some("bound-channel"));
    assert_eq!(record.pane_id.as_ref(), Some(&bound_pane.pane_id));

    let mut provisional = agent("sess-a", Some("lucid-atlas"));
    provisional.agent_id = AgentSessionId::from("launch_pending");
    provisional.channel = None;
    let provisional_pane = pane(
        "claude",
        None,
        None,
        Some("provisional-channel"),
        None,
        "terminal_2",
    );
    let record = agent_record(
        workspace_id.clone(),
        &provisional,
        Some(&provisional_pane),
        "scope",
    );
    assert_eq!(record.agent_id.as_str(), "launch_pending");
    assert_eq!(record.channel.as_deref(), Some("provisional-channel"));
    assert_eq!(record.pane_id.as_ref(), Some(&provisional_pane.pane_id));

    let lazy = pane("codex", None, None, None, Some("/repo/lazy"), "terminal_3");
    let record = pane_record(workspace_id.clone(), &lazy, None, "scope");
    assert_eq!(record.agent_id, synthetic_session_for_pane(&lazy.pane_id));
    assert_eq!(record.channel.as_deref(), Some("lazy"));

    let pane_only = pane(
        "codex",
        Some("pane-session"),
        Some("pane-name"),
        None,
        None,
        "terminal_4",
    );
    let record = pane_record(workspace_id.clone(), &pane_only, None, "scope");
    assert_eq!(record.agent_id.as_str(), "pane-session");
    assert_eq!(record.agent_name.as_deref(), Some("pane-name"));
    assert_eq!(record.channel.as_deref(), Some("scope"));

    let fresh = pane("codex", None, None, None, None, "terminal_5");
    let record = pane_record(workspace_id, &fresh, None, "explicit");
    assert_eq!(record.channel.as_deref(), Some("explicit"));
}

#[test]
fn user_input_requires_plain_human_delivery() {
    let human = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("human", None),
        "prompt".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert!(human.is_user_input());
    assert!(!human.clone().with_automated(true).is_user_input());

    let mut resume = human.clone();
    resume.gate = DeliveryGate::Resume;
    assert!(!resume.is_user_input());
    assert!(
        !human
            .with_sender(agent_sender("coder", None))
            .is_user_input()
    );
}

#[test]
fn delivery_gates_follow_agent_lifecycle() {
    let cases = [
        (AgentStatus::Running, false, false, false),
        (AgentStatus::Waiting, false, false, false),
        (AgentStatus::Idle, true, true, false),
        (AgentStatus::Success, true, true, false),
        (AgentStatus::Failed, false, true, false),
        (AgentStatus::Paused, false, false, true),
    ];
    for (status, done, any, resume) in cases {
        assert_eq!(gate_open(DeliveryGate::Done, status), done, "{status:?}");
        assert_eq!(gate_open(DeliveryGate::Any, status), any, "{status:?}");
        assert_eq!(
            gate_open(DeliveryGate::Resume, status),
            resume,
            "{status:?}"
        );
    }

    let mut running = agent("sess-interrupted", None);
    running.status = AgentStatus::Running;
    running.phase = crate::agents::TurnPhase::Reasoning;
    running.context = Some(settle_context(
        Some(running.last_activity),
        TurnSettleOutcome::Interrupted,
    ));
    let now = Timestamp::now();
    assert!(gate_open_for_agent(
        DeliveryGate::Done,
        &running,
        false,
        now
    ));
    assert!(gate_open_for_agent(DeliveryGate::Any, &running, false, now));

    let mut parked = agent("sess-parked", None);
    parked.status = AgentStatus::Running;
    parked.phase = crate::agents::TurnPhase::Parked;
    assert!(gate_open_for_agent(DeliveryGate::Done, &parked, false, now));
    assert!(gate_open_for_agent(DeliveryGate::Any, &parked, false, now));
    assert!(!gate_open_for_agent(
        DeliveryGate::Resume,
        &parked,
        false,
        now
    ));

    let mut plan = agent("sess-plan", None);
    plan.status = AgentStatus::Running;
    plan.phase = crate::agents::TurnPhase::Reasoning;
    plan.context = Some(settle_context(
        Some(plan.last_activity),
        TurnSettleOutcome::PlanProposed,
    ));
    assert!(plan.is_awaiting_input());
    assert!(!gate_open_for_agent(DeliveryGate::Any, &plan, false, now));

    let mut stale = running.clone();
    stale.last_activity += jiff::SignedDuration::from_secs(2);
    assert!(!gate_open_for_agent(DeliveryGate::Done, &stale, false, now));

    let mut compacting = agent("sess-compacting", None);
    compacting.status = AgentStatus::Idle;
    compacting.compacting_since = Some(now);
    for gate in [DeliveryGate::Done, DeliveryGate::Any, DeliveryGate::Resume] {
        assert!(!gate_open_for_agent(gate, &compacting, true, now));
    }
}

#[test]
fn when_condition_uses_raw_status_and_status_specific_dwell_base() {
    let now = Timestamp::from_second(10_000).unwrap();
    let cases = [
        (AgentStatus::Running, Some(9_900), None, 9_950),
        (AgentStatus::Waiting, None, Some(9_900), 9_950),
        (AgentStatus::Idle, None, None, 9_900),
        (AgentStatus::Success, None, None, 9_900),
        (AgentStatus::Failed, None, None, 9_900),
    ];
    for (status, turn_started, waiting_since, last_activity) in cases {
        let mut watched = agent("watched", Some("coder"));
        watched.status = status;
        watched.turn_started_at = turn_started.map(|secs| Timestamp::from_second(secs).unwrap());
        watched.waiting_since = waiting_since.map(|secs| Timestamp::from_second(secs).unwrap());
        watched.last_activity = Timestamp::from_second(last_activity).unwrap();
        let condition = when_condition(&watched, status, 75, None);
        let snapshot = condition_snapshot(vec![watched]);
        assert!(
            deliver::evaluate_when_condition(&condition, &snapshot, now, Duration::from_secs(30))
                .check
                .met,
            "{status:?}"
        );
    }

    let mut running = agent("running", None);
    running.status = AgentStatus::Running;
    running.last_activity = Timestamp::from_second(9_950).unwrap();
    let condition = when_condition(&running, AgentStatus::Running, 75, None);
    let snapshot = condition_snapshot(vec![running]);
    let check =
        deliver::evaluate_when_condition(&condition, &snapshot, now, Duration::from_secs(30)).check;
    assert!(!check.met);
    assert_eq!(check.trip_at, Some(Timestamp::from_second(10_025).unwrap()));
}

#[test]
fn when_condition_reports_mismatch_gone_and_preserves_latch() {
    let now = Timestamp::from_second(10_000).unwrap();
    let watched = agent("watched", Some("coder"));
    let pending = when_condition(&watched, AgentStatus::Running, 60, None);
    let snapshot = condition_snapshot(vec![watched.clone()]);
    let mismatch =
        deliver::evaluate_when_condition(&pending, &snapshot, now, Duration::from_secs(30));
    assert!(!mismatch.check.met);
    assert_eq!(mismatch.check.trip_at, None);
    let gone = deliver::evaluate_when_condition(
        &pending,
        &condition_snapshot(Vec::new()),
        now,
        Duration::from_secs(30),
    );
    assert_eq!(
        gone.archive_reason.as_deref(),
        Some(pending.expiry_reason().as_str())
    );

    let latched = when_condition(&watched, AgentStatus::Running, 60, Some(now));
    assert!(
        deliver::evaluate_when_condition(
            &latched,
            &condition_snapshot(Vec::new()),
            now,
            Duration::from_secs(30),
        )
        .check
        .met
    );

    let receiver = agent("receiver", None);
    let blocked = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &receiver,
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_after(vec![after_condition(&watched, Some(now))])
    .with_when(vec![pending]);
    assert!(!blocked.is_deliverable(now));
    assert!(blocked.with_when(vec![latched]).is_deliverable(now));
}

#[test]
fn when_parser_accepts_literal_statuses_and_duration_units() {
    for status in ["running", "waiting", "idle", "success", "failed"] {
        assert_eq!(parse_when_status(status).unwrap().as_str(), status);
    }
    assert!(
        parse_when_status("paused")
            .unwrap_err()
            .contains("supported statuses")
    );
    assert_eq!(parse_when_duration("58m").unwrap(), 3_480);
    assert!(
        parse_when_duration("0m")
            .unwrap_err()
            .contains("greater than zero")
    );
}

#[test]
fn delivery_checkpoint_recognizes_turn_boundaries() {
    let checkpoint = crate::agents::DELIVERY_CHECKPOINT;
    assert!(checkpoint.contains(&LifecycleSignal::TurnInterrupted { turn_id: None }));
    assert!(checkpoint.contains(&LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
    }));
    assert!(checkpoint.contains(&LifecycleSignal::TurnEnded {
        errored: true,
        parked_on_background: false,
    }));
    assert!(checkpoint.contains(&LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: true,
    }));
    assert!(!checkpoint.contains(&LifecycleSignal::Registered));
    assert!(checkpoint.contains(&LifecycleSignal::CompactionEnded {
        auto: None,
        failed: false,
    }));
    assert!(checkpoint.contains(&LifecycleSignal::CompactionEnded {
        auto: Some(false),
        failed: true,
    }));
    assert!(!checkpoint.contains(&LifecycleSignal::SubagentStopped { errored: false }));
}

#[test]
fn message_status_classifies_queue_and_terminal_lifecycle() {
    assert!(MessageStatus::Queued.is_open());
    assert!(MessageStatus::Claimed.is_open());
    assert!(!MessageStatus::Sent.is_open());
    assert!(!MessageStatus::Sent.is_terminal());
    for status in [
        MessageStatus::Delivered,
        MessageStatus::TimedOut,
        MessageStatus::Errored,
        MessageStatus::Canceled,
        MessageStatus::Abandoned,
        MessageStatus::Archived,
    ] {
        assert!(status.is_terminal(), "{status}");
    }
    let legacy: MessageStatus = serde_json::from_str("\"pending\"").unwrap();
    assert_eq!(legacy, MessageStatus::Queued);
    let legacy: MessageStatus = serde_json::from_str("\"removed\"").unwrap();
    assert_eq!(legacy, MessageStatus::Canceled);
}

#[test]
fn after_condition_requires_an_open_gate_and_quiescent_ready_queue() {
    let now = Timestamp::now();
    let mut upstream = agent("sess-upstream", Some("planner"));
    let condition = after_condition(&upstream, None);

    for status in [AgentStatus::Running, AgentStatus::Waiting] {
        upstream.status = status;
        assert!(
            !deliver::evaluate_after_condition(
                &condition,
                DeliveryGate::Done,
                &[],
                &condition_snapshot(vec![upstream.clone()]),
                now
            )
            .check
            .met
        );
    }

    upstream.status = AgentStatus::Failed;
    assert!(
        !deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            &[],
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    assert!(
        deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Any,
            &[],
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );

    upstream.status = AgentStatus::Idle;
    let ready = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &upstream,
        "work".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert!(
        !deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&ready),
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    let sent = MessageRecord {
        status: MessageStatus::Sent,
        ..ready.clone()
    };
    assert!(
        !deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&sent),
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    let scheduled = ready.with_not_before(Some(now + jiff::SignedDuration::from_secs(60)));
    assert!(
        deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&scheduled),
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    assert!(
        deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            &[],
            &condition_snapshot(vec![upstream]),
            now
        )
        .check
        .met
    );
}

#[test]
fn auto_compact_parses_percent_and_token_forms() {
    assert_eq!(AutoCompact::parse("70%").unwrap(), AutoCompact::Percent(70));
    assert_eq!(AutoCompact::parse(" 0% ").unwrap(), AutoCompact::Percent(0));
    assert_eq!(
        AutoCompact::parse("120000").unwrap(),
        AutoCompact::Tokens(120_000)
    );
    assert_eq!(
        AutoCompact::parse("180k").unwrap(),
        AutoCompact::Tokens(180_000)
    );
    assert_eq!(
        AutoCompact::parse("2M").unwrap(),
        AutoCompact::Tokens(2_000_000)
    );
    for invalid in ["101%", "18446744073709551615k", "70.5%", "1.5m", "k"] {
        assert!(AutoCompact::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn auto_compact_triggers_from_supported_context_readings() {
    let mut a = agent("s1", None);
    assert!(!AutoCompact::Percent(70).triggered(&a));
    assert!(!AutoCompact::Tokens(1).triggered(&a));

    a.usage.context_pct = Some(75);
    assert!(AutoCompact::Percent(70).triggered(&a));
    assert!(AutoCompact::Percent(75).triggered(&a));
    assert!(!AutoCompact::Percent(76).triggered(&a));

    a.usage.cache_read_input_tokens = Some(100_000);
    a.usage.fresh_input_tokens = Some(20_000);
    assert!(AutoCompact::Tokens(120_000).triggered(&a));
    assert!(!AutoCompact::Tokens(120_001).triggered(&a));

    let mut carried = agent("s2", None);
    carried.usage.total_tokens = Some(120_000);
    carried.usage.context_window = Some(200_000);
    assert!(AutoCompact::Tokens(100_000).triggered(&carried));
    assert!(AutoCompact::Tokens(120_000).triggered(&carried));
    assert!(!AutoCompact::Tokens(120_001).triggered(&carried));
}

#[test]
fn message_record_round_trips_current_schema_and_reads_legacy_defaults() {
    let now = Timestamp::from_second(1_000).unwrap();
    let receiver = agent("s1", Some("lucid-atlas"));
    let upstream = agent("s2", Some("planner"));
    let mut record = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &receiver,
        "next".to_owned(),
        false,
        DeliveryGate::Any,
    )
    .with_address(Some("@coder#docs".to_owned()))
    .with_channel(Some("docs".to_owned()))
    .with_sender(MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("lucid-atlas".to_owned()),
        profile: Some("planner".to_owned()),
        role: Some("coder".to_owned()),
        channel: Some("main".to_owned()),
    })
    .with_in_reply_to(vec![message_id(7), message_id(8)])
    .with_automated(true)
    .with_reply_wait(true)
    .with_body(MessageBody::Command)
    .with_force(true)
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    .with_not_before(Some(now + jiff::SignedDuration::from_secs(60)))
    .with_after(vec![after_condition(&upstream, Some(now))])
    .with_auto_compact(Some(AutoCompact::Percent(70)));
    record.status = MessageStatus::Delivered;
    record.enqueued_at = now - jiff::SignedDuration::from_secs(120);
    record.updated_at = now;
    record.attempts = 3;
    record.unconfirmed_sends = 2;
    record.last_attempt_at = Some(now - jiff::SignedDuration::from_secs(5));
    record.last_error = Some("pane unavailable".to_owned());
    record.delivered_at = Some(now + jiff::SignedDuration::from_secs(1));
    record.retry_after = Some(now + jiff::SignedDuration::from_secs(30));
    record.compacted_context_tokens = Some(150_000);
    record.batch_id = Some(message_id(1));

    let json = serde_json::to_string(&record).unwrap();
    let back: MessageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back, record);

    let mut legacy = serde_json::to_value(&record).unwrap();
    for key in [
        "in_reply_to",
        "agent_name",
        "address",
        "channel",
        "sender",
        "automated",
        "reply_wait",
        "body",
        "force",
        "pane_id",
        "attempts",
        "unconfirmed_sends",
        "last_attempt_at",
        "last_error",
        "delivered_at",
        "not_before",
        "after",
        "when",
        "retry_after",
        "auto_compact",
        "compacted_context_tokens",
        "batch_id",
    ] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    let legacy: MessageRecord = serde_json::from_value(legacy).unwrap();
    assert!(legacy.in_reply_to.is_empty());
    assert_eq!(legacy.agent_name, None);
    assert_eq!(legacy.address, None);
    assert_eq!(legacy.channel, None);
    assert_eq!(legacy.sender, MessageSender::Human);
    assert!(!legacy.automated);
    assert!(!legacy.reply_wait);
    assert_eq!(legacy.body, MessageBody::Prompt);
    assert!(!legacy.force);
    assert_eq!(legacy.pane_id, None);
    assert_eq!(legacy.attempts, 0);
    assert_eq!(legacy.unconfirmed_sends, 0);
    assert_eq!(legacy.last_attempt_at, None);
    assert_eq!(legacy.last_error, None);
    assert_eq!(legacy.delivered_at, None);
    assert_eq!(legacy.not_before, None);
    assert!(legacy.after.is_empty());
    assert!(legacy.when.is_empty());
    assert_eq!(legacy.retry_after, None);
    assert_eq!(legacy.auto_compact, None);
    assert_eq!(legacy.compacted_context_tokens, None);
    assert_eq!(legacy.batch_id, None);
    assert_eq!(legacy.status, MessageStatus::Delivered);
    assert_eq!(legacy.text, "next");
}

#[test]
fn requeue_preserves_intent_and_rearms_dependencies() {
    let now = Timestamp::from_second(1_000).unwrap();
    let receiver = agent("s1", Some("coder"));
    let mut upstream = agent("s2", Some("planner"));
    upstream.status = AgentStatus::Running;
    let mut original = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &receiver,
        "next".to_owned(),
        false,
        DeliveryGate::Any,
    )
    .with_address(Some("@coder#docs".to_owned()))
    .with_channel(Some("docs".to_owned()))
    .with_sender(agent_sender("reviewer", Some("docs")))
    .with_in_reply_to(vec![message_id(7), message_id(8)])
    .with_automated(true)
    .with_reply_wait(true)
    .with_body(MessageBody::Command)
    .with_force(true)
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    .with_not_before(Some(now - jiff::SignedDuration::from_secs(60)))
    .with_after(vec![after_condition(&upstream, Some(now))])
    .with_when(vec![when_condition(
        &upstream,
        AgentStatus::Running,
        60,
        Some(now),
    )])
    .with_auto_compact(Some(AutoCompact::Tokens(120_000)));
    original.status = MessageStatus::Errored;
    original.enqueued_at = now - jiff::SignedDuration::from_secs(120);
    original.updated_at = now;
    original.attempts = 4;
    original.unconfirmed_sends = 2;
    original.last_attempt_at = Some(now);
    original.last_error = Some("pane closed".to_owned());
    original.delivered_at = Some(now);
    original.retry_after = Some(now + jiff::SignedDuration::from_secs(30));
    original.compacted_context_tokens = Some(120_000);
    original.batch_id = Some(message_id(1));

    assert!(original.is_deliverable(now));
    let latched = deliver::evaluate_after_condition(
        &original.after[0],
        original.gate,
        &[],
        &condition_snapshot(vec![upstream.clone()]),
        now,
    );
    assert!(latched.check.met);
    assert!(!latched.stamp_needed);

    let requeued = MessageRecord::requeue_from(&original);

    assert_ne!(requeued.message_id, original.message_id);
    assert_eq!(requeued.workspace_id, original.workspace_id);
    assert_eq!(requeued.kind, original.kind);
    assert_eq!(requeued.agent_id, original.agent_id);
    assert_eq!(requeued.agent_name, original.agent_name);
    assert_eq!(requeued.address, original.address);
    assert_eq!(requeued.channel, original.channel);
    assert_eq!(requeued.sender, original.sender);
    assert_eq!(requeued.body, original.body);
    assert_eq!(requeued.text, original.text);
    assert_eq!(requeued.enter, original.enter);
    assert_eq!(requeued.gate, original.gate);
    assert_eq!(requeued.force, original.force);
    assert_eq!(requeued.not_before, original.not_before);
    assert_eq!(requeued.auto_compact, original.auto_compact);
    assert_eq!(requeued.in_reply_to, original.in_reply_to);
    assert!(requeued.automated);
    assert!(!requeued.reply_wait);
    let mut expected_after = original.after.clone();
    for condition in &mut expected_after {
        condition.met_at = None;
    }
    assert_eq!(requeued.after, expected_after);
    let mut expected_when = original.when.clone();
    for condition in &mut expected_when {
        condition.met_at = None;
    }
    assert_eq!(requeued.when, expected_when);
    assert!(!requeued.is_deliverable(now));
    assert_eq!(requeued.status, MessageStatus::Queued);
    assert_eq!(requeued.enqueued_at, requeued.updated_at);
    assert_eq!(requeued.attempts, 0);
    assert_eq!(requeued.unconfirmed_sends, 0);
    assert_eq!(requeued.last_attempt_at, None);
    assert_eq!(requeued.last_sent_at, None);
    assert_eq!(requeued.last_error, None);
    assert_eq!(requeued.delivered_at, None);
    assert_eq!(requeued.retry_after, None);
    assert_eq!(requeued.pane_id, None);
    assert_eq!(requeued.batch_id, None);
    assert_eq!(requeued.compacted_context_tokens, None);
}

#[test]
fn wake_deadline_arms_queue_retry_schedule_and_sent_reconciliation() {
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
    let window = MessageBody::Prompt.delivery_window();
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
        (
            MessageRecord {
                status: MessageStatus::Delivered,
                ..message
            },
            None,
        ),
    ];
    for (message, expected) in cases {
        assert_eq!(message.wake_deadline(now), expected);
    }
}

#[test]
fn delivery_policy_is_per_body_and_sent_time_survives_legacy_records() {
    assert_eq!(
        MessageBody::Prompt.delivery_window(),
        delivery_window_from_env()
    );
    assert_eq!(
        MessageBody::Command.delivery_window(),
        env_ms(COMMAND_DELIVERY_WINDOW_ENV).unwrap_or(DEFAULT_COMMAND_DELIVERY_WINDOW)
    );
    assert!(MessageBody::Prompt.resends_unconfirmed());
    assert!(!MessageBody::Command.resends_unconfirmed());

    let updated_at = Timestamp::from_second(1_000).unwrap();
    let last_sent_at = updated_at + jiff::SignedDuration::from_secs(10);
    let mut sent = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("legacy", None),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    sent.status = MessageStatus::Sent;
    sent.updated_at = updated_at;
    assert_eq!(
        sent.sent_reconcile_deadline(),
        Some(updated_at + MessageBody::Prompt.delivery_window())
    );
    sent.last_sent_at = Some(last_sent_at);
    assert_eq!(
        sent.sent_reconcile_deadline(),
        Some(last_sent_at + MessageBody::Prompt.delivery_window())
    );
    let mut queued = sent.clone();
    queued.status = MessageStatus::Queued;
    queued.unconfirmed_sends = 1;
    let late_ack_deadline = last_sent_at + MessageBody::Prompt.delivery_window() * 2;
    assert!(queued.awaiting_late_ack(last_sent_at + MessageBody::Prompt.delivery_window()));
    assert!(queued.awaiting_late_ack(late_ack_deadline));
    assert!(!queued.awaiting_late_ack(late_ack_deadline + Duration::from_nanos(1)));

    let mut value = serde_json::to_value(&sent).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("last_sent_at")
        .expect("serialized field");
    let legacy: MessageRecord = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.last_sent_at, None);
    assert_eq!(
        legacy.sent_reconcile_deadline(),
        Some(updated_at + MessageBody::Prompt.delivery_window())
    );
}

#[test]
fn sender_render_uses_attributed_address_precedence() {
    let cases = [
        (MessageSender::Human, "you"),
        (MessageSender::System, "rimz"),
        (
            MessageSender::Agent {
                kind: AgentKind::new_unchecked("claude"),
                name: Some("lucid-atlas".to_owned()),
                profile: Some("planner".to_owned()),
                role: Some("coder".to_owned()),
                channel: Some("docs".to_owned()),
            },
            "@coder#docs",
        ),
        (
            MessageSender::Agent {
                kind: AgentKind::new_unchecked("claude"),
                name: Some("lucid-atlas".to_owned()),
                profile: Some("planner".to_owned()),
                role: None,
                channel: None,
            },
            "@planner",
        ),
        (
            MessageSender::Agent {
                kind: AgentKind::new_unchecked("codex"),
                name: None,
                profile: None,
                role: None,
                channel: None,
            },
            "@codex",
        ),
    ];
    for (sender, expected) in cases {
        assert_eq!(sender.render(), expected);
    }
}

#[test]
fn queue_head_selects_oldest_deliverable_record_per_lane() {
    let now = Timestamp::now();
    let ws = WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message"));
    {
        let provisional = agent("launch_1", Some("lucid-atlas"));
        let registered = agent("real-session", Some("lucid-atlas"));
        let mut older = MessageRecord::new(
            ws.clone(),
            &provisional,
            "first".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let mut newer = MessageRecord::new(
            ws.clone(),
            &registered,
            "second".to_owned(),
            true,
            DeliveryGate::Done,
        );
        older.message_id = message_id(1);
        newer.message_id = message_id(2);
        let pending = [newer.clone(), older.clone()];

        assert!(older.same_agent_card(&registered));
        assert_eq!(
            queue_head(
                pending.iter(),
                &registered.kind,
                &registered.agent_id,
                registered.name.as_deref(),
                now,
            )
            .unwrap()
            .message_id,
            older.message_id
        );
        assert_eq!(
            queue_head(
                pending.iter(),
                &registered.kind,
                &registered.agent_id,
                None,
                now,
            )
            .unwrap()
            .message_id,
            newer.message_id
        );
    }

    {
        let receiver = agent("real-session", Some("lucid-atlas"));
        let mut future = MessageRecord::new(
            ws.clone(),
            &receiver,
            "later".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_not_before(Some(now + jiff::SignedDuration::from_secs(60)));
        let mut ready = MessageRecord::new(
            ws.clone(),
            &receiver,
            "now".to_owned(),
            true,
            DeliveryGate::Done,
        );
        future.message_id = message_id(1);
        ready.message_id = message_id(2);
        let pending = [future, ready.clone()];

        assert_eq!(
            queue_head(
                pending.iter(),
                &receiver.kind,
                &receiver.agent_id,
                receiver.name.as_deref(),
                now,
            )
            .unwrap()
            .message_id,
            ready.message_id
        );
    }

    {
        let receiver = agent("sess-receiver", Some("coder"));
        let upstream = agent("sess-upstream", Some("planner"));
        let mut waiting = MessageRecord::new(
            ws.clone(),
            &receiver,
            "after planner".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_after(vec![after_condition(&upstream, None)]);
        let mut ready = MessageRecord::new(
            ws.clone(),
            &receiver,
            "plain".to_owned(),
            true,
            DeliveryGate::Done,
        );
        waiting.message_id = message_id(1);
        ready.message_id = message_id(2);
        let pending = [waiting.clone(), ready.clone()];

        assert!(!waiting.is_deliverable(now));
        assert_eq!(
            queue_head(
                pending.iter(),
                &receiver.kind,
                &receiver.agent_id,
                receiver.name.as_deref(),
                now,
            )
            .unwrap()
            .message_id,
            ready.message_id
        );
    }

    {
        let receiver = agent("real-session", Some("lucid-atlas"));
        let mut deferred = MessageRecord::new(
            ws.clone(),
            &receiver,
            "old".to_owned(),
            true,
            DeliveryGate::Done,
        );
        deferred.message_id = message_id(1);
        deferred.retry_after = Some(now + jiff::SignedDuration::from_secs(60));
        let mut newer =
            MessageRecord::new(ws, &receiver, "new".to_owned(), true, DeliveryGate::Done);
        newer.message_id = message_id(2);
        let pending = [deferred.clone(), newer];

        assert_eq!(
            queue_head(
                pending.iter(),
                &receiver.kind,
                &receiver.agent_id,
                receiver.name.as_deref(),
                now,
            )
            .unwrap()
            .message_id,
            deferred.message_id
        );
    }
}

#[test]
fn schedule_parser_accepts_durations_and_rolls_wall_clock_forward() {
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

#[test]
fn delivery_batch_selector_pins_fifo_lanes_and_compatible_prefix() {
    let now = Timestamp::from_second(1_700_000_100).unwrap();
    let receiver = agent("receiver", Some("coder"));
    let other = agent("other", Some("reviewer"));

    let head = delivery_message(2, &receiver, DeliveryGate::Done, Some("same"));
    assert!(delivery_batch_indices(&[], &head.message_id, AgentStatus::Idle, now).is_none());
    let mut sent = head.clone();
    sent.status = MessageStatus::Sent;
    assert!(delivery_batch_indices(&[sent], &head.message_id, AgentStatus::Idle, now).is_none());
    let mut recently_claimed = head.clone();
    recently_claimed.last_attempt_at = Some(now);
    assert!(
        delivery_batch_indices(
            &[recently_claimed],
            &head.message_id,
            AgentStatus::Idle,
            now,
        )
        .is_none()
    );

    for blocker_status in [MessageStatus::Queued, MessageStatus::Claimed] {
        let mut blocker = delivery_message(1, &receiver, DeliveryGate::Any, Some("same"));
        blocker.status = blocker_status;
        assert!(
            delivery_batch_indices(
                &[blocker, head.clone()],
                &head.message_id,
                AgentStatus::Idle,
                now,
            )
            .is_none(),
            "older {blocker_status:?} same-lane record blocks"
        );
    }

    let unrelated_card = delivery_message(1, &other, DeliveryGate::Done, Some("same"));
    let unrelated_lane = delivery_message(2, &receiver, DeliveryGate::Resume, Some("same"));
    let mut future = delivery_message(3, &receiver, DeliveryGate::Done, Some("same"));
    future.not_before = Some(now + Duration::from_secs(60));
    let head = delivery_message(4, &receiver, DeliveryGate::Done, Some("same"));
    let compatible = delivery_message(5, &receiver, DeliveryGate::Any, Some("same"));
    let mut blocked = delivery_message(6, &receiver, DeliveryGate::Done, Some("same"));
    blocked.after.push(AfterCondition {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: "upstream".into(),
        agent_name: None,
        address: "@upstream".to_owned(),
        met_at: None,
    });
    let after_blocked = delivery_message(7, &receiver, DeliveryGate::Done, Some("same"));
    let barrier = delivery_message(8, &receiver, DeliveryGate::Done, Some("other"));
    let after_barrier = delivery_message(9, &receiver, DeliveryGate::Done, Some("same"));
    let live = vec![
        unrelated_card,
        unrelated_lane,
        future,
        head.clone(),
        compatible,
        blocked,
        after_blocked,
        barrier,
        after_barrier,
    ];
    let selected = delivery_batch_indices(&live, &head.message_id, AgentStatus::Idle, now).unwrap();
    assert_eq!(selected, vec![3, 4, 6]);

    let head = delivery_message(1, &receiver, DeliveryGate::Done, None);
    let mut expired = delivery_message(2, &receiver, DeliveryGate::Done, None);
    expired.status = MessageStatus::Claimed;
    expired.last_attempt_at = Some(now - CLAIM_TTL);
    let mut unexpired = delivery_message(3, &receiver, DeliveryGate::Done, None);
    unexpired.status = MessageStatus::Claimed;
    unexpired.last_attempt_at = Some(now);
    let tail = delivery_message(4, &receiver, DeliveryGate::Done, None);
    assert_eq!(
        delivery_batch_indices(
            &[head.clone(), expired, unexpired, tail],
            &head.message_id,
            AgentStatus::Idle,
            now,
        )
        .unwrap(),
        vec![0, 1]
    );

    let resume = delivery_message(1, &receiver, DeliveryGate::Resume, None);
    let resume_tail = delivery_message(2, &receiver, DeliveryGate::Resume, None);
    assert_eq!(
        delivery_batch_indices(
            &[resume.clone(), resume_tail],
            &resume.message_id,
            AgentStatus::Paused,
            now,
        )
        .unwrap(),
        vec![0]
    );

    let head = delivery_message(1, &receiver, DeliveryGate::Any, None);
    let closed_gate = delivery_message(2, &receiver, DeliveryGate::Done, None);
    let tail = delivery_message(3, &receiver, DeliveryGate::Any, None);
    assert_eq!(
        delivery_batch_indices(
            &[head.clone(), closed_gate, tail],
            &head.message_id,
            AgentStatus::Failed,
            now,
        )
        .unwrap(),
        vec![0]
    );
}

fn message_id(value: u64) -> MessageId {
    MessageId::parse(&format!("msg_{value:016}")).unwrap()
}

fn delivery_message(
    id: u64,
    agent: &AgentState,
    gate: DeliveryGate,
    channel: Option<&str>,
) -> MessageRecord {
    let mut message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        agent,
        format!("message {id}"),
        true,
        gate,
    )
    .with_channel(channel.map(ToOwned::to_owned));
    message.message_id = message_id(id);
    message
}

fn agent_sender(role: &str, channel: Option<&str>) -> MessageSender {
    MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: None,
        profile: None,
        role: Some(role.to_owned()),
        channel: channel.map(ToOwned::to_owned),
    }
}

fn agent(id: &str, name: Option<&str>) -> AgentState {
    let mut agent = AgentState::stub("claude", id, AgentStatus::Idle);
    agent.name = name.map(ToOwned::to_owned);
    agent
}

fn draft(not_before: Option<Timestamp>) -> MessageDraft {
    MessageDraft {
        body: MessageBody::Prompt,
        enter: true,
        gate: DeliveryGate::Done,
        sender: MessageSender::Human,
        automated: false,
        force: false,
        auto_compact: None,
        not_before,
        after: Vec::new(),
        when: Vec::new(),
    }
}

fn agent_record(
    workspace_id: WorkspaceId,
    agent: &AgentState,
    pane: Option<&PaneAgent>,
    channel: &str,
) -> MessageRecord {
    draft(None).record(
        workspace_id,
        Recipient::Agent { agent, pane },
        Some(channel),
        "hello",
        Some("@claude"),
    )
}

fn pane_record(
    workspace_id: WorkspaceId,
    pane: &PaneAgent,
    bound: Option<&AgentState>,
    channel: &str,
) -> MessageRecord {
    draft(None).record(
        workspace_id,
        Recipient::Pane { pane, bound },
        Some(channel),
        "hello",
        Some("@claude"),
    )
}

fn pane(
    kind: &str,
    agent_id: Option<&str>,
    name: Option<&str>,
    channel: Option<&str>,
    worktree_path: Option<&str>,
    raw: &str,
) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked(kind),
        kind_ordinal: None,
        name: name.map(ToOwned::to_owned),
        name_explicit: false,
        profile: None,
        role: None,
        channel: channel.map(ToOwned::to_owned),
        agent_id: agent_id.map(AgentSessionId::from),
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        pane_pid: None,
        worktree_path: worktree_path.map(ToOwned::to_owned),
        worktree_branch: None,
    }
}

fn condition_snapshot(agents: Vec<AgentState>) -> crate::store::snapshot::SidebarSnapshot {
    crate::store::snapshot::SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        agents,
        Timestamp::now(),
    )
}

fn after_condition(agent: &AgentState, met_at: Option<Timestamp>) -> AfterCondition {
    AfterCondition {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        agent_name: agent.name.clone(),
        address: format!("@{}", agent.name.as_deref().unwrap_or(agent.kind.as_str())),
        met_at,
    }
}

fn when_condition(
    agent: &AgentState,
    status: AgentStatus,
    dwell_secs: u64,
    met_at: Option<Timestamp>,
) -> WhenCondition {
    WhenCondition {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        agent_name: agent.name.clone(),
        address: format!("@{}", agent.name.as_deref().unwrap_or(agent.kind.as_str())),
        status,
        dwell_secs,
        met_at,
    }
}

/// A Codex sidecar whose resting marker postdates `after` by one second.
fn settle_context(after: Option<Timestamp>, outcome: TurnSettleOutcome) -> AgentContext {
    AgentContext {
        settle: after.map(|at| TurnSettle::new(at + jiff::SignedDuration::from_secs(1), outcome)),
        ..AgentContext::new("codex", Timestamp::now())
    }
}
