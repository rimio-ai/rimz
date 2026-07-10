use std::time::Duration;

use jiff::Timestamp;

use super::*;
use crate::agents::{AgentContext, AgentState, AgentStatus};
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

    let mut running = agent("sess-interrupted", None);
    running.status = AgentStatus::Running;
    running.phase = crate::agents::TurnPhase::Reasoning;
    running.context = Some(settle_context(None, Some(running.last_activity)));
    assert!(gate_open_for_agent(DeliveryGate::Done, &running, false));
    assert!(gate_open_for_agent(DeliveryGate::Any, &running, false));

    let mut stale = running.clone();
    stale.last_activity += jiff::SignedDuration::from_secs(2);
    assert!(!gate_open_for_agent(DeliveryGate::Done, &stale, false));
}

#[test]
fn delivery_checkpoint_requires_unparked_turn_end() {
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
fn message_status_classifies_queue_and_terminal_lifecycle() {
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
fn after_condition_requires_an_open_gate_and_quiescent_ready_queue() {
    let now = Timestamp::now();
    let mut upstream = agent("sess-upstream", Some("planner"));
    let condition = after_condition(&upstream, None);

    for status in [AgentStatus::Running, AgentStatus::Waiting] {
        upstream.status = status;
        assert!(!after_condition_open(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&upstream),
            &[],
            now
        ));
    }

    upstream.status = AgentStatus::Failed;
    assert!(!after_condition_open(
        &condition,
        DeliveryGate::Done,
        std::slice::from_ref(&upstream),
        &[],
        now
    ));
    assert!(after_condition_open(
        &condition,
        DeliveryGate::Any,
        std::slice::from_ref(&upstream),
        &[],
        now
    ));

    upstream.status = AgentStatus::Idle;
    let ready = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &upstream,
        "work".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert!(!after_condition_open(
        &condition,
        DeliveryGate::Done,
        std::slice::from_ref(&upstream),
        std::slice::from_ref(&ready),
        now
    ));
    let sent = MessageRecord {
        status: MessageStatus::Sent,
        ..ready.clone()
    };
    assert!(!after_condition_open(
        &condition,
        DeliveryGate::Done,
        std::slice::from_ref(&upstream),
        std::slice::from_ref(&sent),
        now
    ));
    let scheduled = ready.with_not_before(Some(now + jiff::SignedDuration::from_secs(60)));
    assert!(after_condition_open(
        &condition,
        DeliveryGate::Done,
        std::slice::from_ref(&upstream),
        std::slice::from_ref(&scheduled),
        now
    ));
    assert!(after_condition_open(
        &condition,
        DeliveryGate::Done,
        std::slice::from_ref(&upstream),
        &[],
        now
    ));
}

#[test]
fn claim_ttl_expires_at_boundary_and_on_clock_skew() {
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

    a.context_pct = Some(75);
    assert!(AutoCompact::Percent(70).triggered(&a));
    assert!(AutoCompact::Percent(75).triggered(&a));
    assert!(!AutoCompact::Percent(76).triggered(&a));

    a.cache_read_input_tokens = Some(100_000);
    a.fresh_input_tokens = Some(20_000);
    assert!(AutoCompact::Tokens(120_000).triggered(&a));
    assert!(!AutoCompact::Tokens(120_001).triggered(&a));

    let mut carried = agent("s2", None);
    carried.total_tokens = Some(120_000);
    carried.context_window = Some(200_000);
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
    assert!(!after_condition_open(
        &original.after[0],
        original.gate,
        std::slice::from_ref(&upstream),
        &[],
        now
    ));

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
    assert!(!requeued.is_deliverable(now));
    assert_eq!(requeued.status, MessageStatus::Queued);
    assert_eq!(requeued.enqueued_at, requeued.updated_at);
    assert_eq!(requeued.attempts, 0);
    assert_eq!(requeued.unconfirmed_sends, 0);
    assert_eq!(requeued.last_attempt_at, None);
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
fn sender_render_uses_attributed_address_precedence() {
    let cases = [
        (MessageSender::Human, "you"),
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
        let mut ordinary = MessageRecord::new(
            ws.clone(),
            &receiver,
            "ordinary".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let mut resume = MessageRecord::new(
            ws.clone(),
            &receiver,
            "continue".to_owned(),
            true,
            DeliveryGate::Resume,
        );
        ordinary.message_id = message_id(1);
        resume.message_id = message_id(2);
        let pending = [ordinary.clone(), resume.clone()];

        assert_eq!(
            queue_head_for_message(pending.iter(), &ordinary, now)
                .unwrap()
                .message_id,
            ordinary.message_id
        );
        assert_eq!(
            queue_head_for_message(pending.iter(), &resume, now)
                .unwrap()
                .message_id,
            resume.message_id
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
fn batching_collects_compatible_human_and_agent_prompts() {
    let receiver = agent("real-session", Some("lucid-atlas"));
    let now = Timestamp::now();

    let human = batch_message(&receiver, 1, "human");
    let agent_authored =
        batch_message(&receiver, 2, "agent").with_sender(agent_sender("coder", Some("main")));
    let pending = [agent_authored];
    let tail = queue_batch_tail(pending.iter(), &human, AgentStatus::Idle, now);
    assert_eq!(tail[0].message_id, message_id(2));

    let head =
        batch_message(&receiver, 1, "first").with_sender(agent_sender("planner", Some("main")));
    let second =
        batch_message(&receiver, 2, "second").with_sender(agent_sender("coder", Some("main")));
    let third =
        batch_message(&receiver, 3, "third").with_sender(agent_sender("reviewer", Some("main")));
    let cross =
        batch_message(&receiver, 4, "cross").with_sender(agent_sender("reviewer", Some("docs")));
    let later_same =
        batch_message(&receiver, 5, "later").with_sender(agent_sender("designer", Some("main")));
    let pending = [later_same, cross, third, second];
    let tail = queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now);
    let ids: Vec<_> = tail
        .iter()
        .map(|message| message.message_id.clone())
        .collect();
    assert_eq!(ids, vec![message_id(2), message_id(3)]);
}

#[test]
fn batching_stops_at_barriers_but_skips_control_and_future_records() {
    let receiver = agent("real-session", Some("lucid-atlas"));
    let now = Timestamp::now();
    let blockers = [
        MessageRecord {
            text: "/compact".to_owned(),
            ..batch_message(&receiver, 2, "slash")
        },
        batch_message(&receiver, 2, "command").with_body(MessageBody::Command),
        MessageRecord {
            enter: false,
            ..batch_message(&receiver, 2, "draft")
        },
    ];
    for blocker in blockers {
        let head = batch_message(&receiver, 1, "first");
        let later = batch_message(&receiver, 3, "later");
        let pending = [blocker, later];
        assert!(
            queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now).is_empty(),
            "a non-batchable follower stops the prefix"
        );
    }

    let non_batchable_heads = [
        MessageRecord {
            text: "/compact".to_owned(),
            ..batch_message(&receiver, 1, "slash")
        },
        batch_message(&receiver, 1, "command").with_body(MessageBody::Command),
        MessageRecord {
            enter: false,
            ..batch_message(&receiver, 1, "draft")
        },
    ];
    for head in non_batchable_heads {
        let pending = [batch_message(&receiver, 2, "later")];
        assert!(queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now).is_empty());
    }

    let head = batch_message(&receiver, 1, "first");
    let middle =
        batch_message(&receiver, 2, "middle").with_sender(agent_sender("coder", Some("docs")));
    let later = batch_message(&receiver, 3, "later");
    let pending = [middle, later];
    assert!(queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now).is_empty());

    let head = batch_message(&receiver, 1, "first").with_force(true);
    let middle = batch_message(&receiver, 2, "middle");
    let later = batch_message(&receiver, 3, "later").with_force(true);
    let pending = [middle, later];
    assert!(queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now).is_empty());

    let head = MessageRecord {
        gate: DeliveryGate::Any,
        ..batch_message(&receiver, 1, "first")
    };
    let middle = MessageRecord {
        gate: DeliveryGate::Done,
        ..batch_message(&receiver, 2, "middle")
    };
    let later = MessageRecord {
        gate: DeliveryGate::Any,
        ..batch_message(&receiver, 3, "later")
    };
    let pending = [middle, later];
    assert!(queue_batch_tail(pending.iter(), &head, AgentStatus::Failed, now).is_empty());

    let head = batch_message(&receiver, 1, "first");
    let resume = MessageRecord {
        gate: DeliveryGate::Resume,
        ..batch_message(&receiver, 2, "continue")
    };
    let ready = batch_message(&receiver, 3, "ready");
    let pending = [resume, ready];
    let tail = queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now);
    assert_eq!(tail[0].message_id, message_id(3));

    let resume_head = MessageRecord {
        gate: DeliveryGate::Resume,
        ..batch_message(&receiver, 1, "continue")
    };
    let pending = [batch_message(&receiver, 2, "ordinary")];
    assert!(queue_batch_tail(pending.iter(), &resume_head, AgentStatus::Paused, now).is_empty());

    let head = batch_message(&receiver, 1, "first");
    let future = batch_message(&receiver, 2, "future")
        .with_not_before(Some(now + jiff::SignedDuration::from_secs(60)));
    let ready = batch_message(&receiver, 3, "ready");
    let pending = [future, ready];
    let tail = queue_batch_tail(pending.iter(), &head, AgentStatus::Idle, now);
    assert_eq!(tail[0].message_id, message_id(3));
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

fn message_id(value: u64) -> MessageId {
    MessageId::parse(&format!("msg_{value:016}")).unwrap()
}

fn batch_message(agent: &AgentState, id: u64, text: &str) -> MessageRecord {
    let mut message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        agent,
        text.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_channel(Some("main".to_owned()));
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

fn after_condition(agent: &AgentState, met_at: Option<Timestamp>) -> AfterCondition {
    AfterCondition {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        agent_name: agent.name.clone(),
        address: format!("@{}", agent.name.as_deref().unwrap_or(agent.kind.as_str())),
        met_at,
    }
}

fn settle_context(complete: Option<Timestamp>, interrupted: Option<Timestamp>) -> AgentContext {
    AgentContext {
        source: "codex".to_owned(),
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
        turn_error: None,
        turn_complete: complete.map(|at| at + jiff::SignedDuration::from_secs(1)),
        turn_interrupted: interrupted.map(|at| at + jiff::SignedDuration::from_secs(1)),
        observed_at: Timestamp::now(),
    }
}
