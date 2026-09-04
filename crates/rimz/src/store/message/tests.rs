use std::time::Duration;

use jiff::Timestamp;

use super::super::super::message::deliver;
use super::*;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, MessageId, MuxName, PaneId, WorkspaceId};

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
        env_ms(DELIVERY_WINDOW_ENV).unwrap_or(DEFAULT_DELIVERY_WINDOW)
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
    assert!(queued.awaiting_late_ack());
    queued.last_sent_at = None;
    assert!(!queued.awaiting_late_ack());

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
            MessageSender::Harness {
                notice: HarnessNotice::SubagentReport,
            },
            "@rimz",
        ),
        (
            MessageSender::Subagent {
                kind: AgentKind::new_unchecked("codex"),
                name: "lucid-atlas".to_owned(),
            },
            "@lucid-atlas",
        ),
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
fn subagent_sender_round_trips_with_its_own_origin() {
    let sender = MessageSender::Subagent {
        kind: AgentKind::new_unchecked("codex"),
        name: "lucid-atlas".to_owned(),
    };
    let json = serde_json::to_value(&sender).unwrap();

    assert_eq!(json["origin"], "subagent");
    assert_eq!(
        serde_json::from_value::<MessageSender>(json).unwrap(),
        sender
    );
}

#[test]
fn harness_sender_round_trips_with_stable_notice_wire() {
    let sender = MessageSender::Harness {
        notice: HarnessNotice::SubagentReport,
    };
    let json = serde_json::to_value(&sender).unwrap();

    assert_eq!(json["origin"], "harness");
    assert_eq!(json["notice"], "subagent_report");
    assert_eq!(
        serde_json::from_value::<MessageSender>(json).unwrap(),
        sender
    );
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

pub(crate) fn agent_sender(role: &str, channel: Option<&str>) -> MessageSender {
    MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: None,
        profile: None,
        role: Some(role.to_owned()),
        channel: channel.map(ToOwned::to_owned),
    }
}

pub(crate) fn agent(id: &str, name: Option<&str>) -> AgentState {
    let mut agent = AgentState::stub("claude", id, AgentStatus::Idle);
    agent.name = name.map(ToOwned::to_owned);
    agent
}

pub(crate) fn condition_snapshot(
    agents: Vec<AgentState>,
) -> crate::store::snapshot::SidebarSnapshot {
    crate::store::snapshot::SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        agents,
        Timestamp::now(),
    )
}

pub(crate) fn after_condition(agent: &AgentState, met_at: Option<Timestamp>) -> AfterCondition {
    AfterCondition {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        agent_name: agent.name.clone(),
        address: format!("@{}", agent.name.as_deref().unwrap_or(agent.kind.as_str())),
        met_at,
    }
}

pub(crate) fn when_condition(
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
