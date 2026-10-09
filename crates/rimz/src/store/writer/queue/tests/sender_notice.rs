use super::*;
use crate::store::message::{HarnessNotice, sender_notice};

#[test]
fn sender_resolution_uses_live_launch_or_session_identity_then_legacy_name() {
    let mut card = agent();
    card.agent_id = "sender-session".into();
    card.launch_id = Some("sender-launch".into());
    card.name = Some("sender".into());
    let mut origin = sender();
    for identity in [Some("sender-session"), Some("sender-launch"), None] {
        if let MessageSender::Agent { agent_id, .. } = &mut origin {
            *agent_id = identity.map(Into::into);
        }
        assert_eq!(
            sender_notice::resolve_sender(&origin, std::slice::from_ref(&card))
                .map(|agent| &agent.agent_id),
            Some(&card.agent_id)
        );
    }
    for (kind, identity, name, ended) in [
        ("claude", Some("old-launch"), Some("sender"), false),
        ("codex", Some("sender-launch"), Some("sender"), false),
        ("claude", None, None, false),
        ("claude", None, Some("someone-else"), false),
        ("claude", Some("sender-session"), Some("sender"), true),
    ] {
        let mut card = card.clone();
        card.ended_at = ended.then(Timestamp::now);
        let origin = MessageSender::Agent {
            kind: AgentKind::new_unchecked(kind),
            agent_id: identity.map(Into::into),
            name: name.map(Into::into),
            profile: None,
            role: None,
            channel: None,
        };
        assert!(sender_notice::resolve_sender(&origin, &[card]).is_none());
    }
}

#[test]
fn a_name_shared_across_lanes_resolves_by_the_senders_channel() {
    let lane = |session: &str, channel: &str| {
        let mut card = agent();
        card.agent_id = session.into();
        card.name = Some("sender".into());
        card.channel = Some(channel.into());
        card
    };
    let cards = [lane("docs-session", "docs"), lane("auth-session", "auth")];
    let origin = |channel: Option<&str>| MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: None,
        name: Some("sender".into()),
        profile: None,
        role: None,
        channel: channel.map(Into::into),
    };
    assert_eq!(
        sender_notice::resolve_sender(&origin(Some("auth")), &cards)
            .map(|agent| agent.agent_id.as_str()),
        Some("auth-session")
    );
    assert!(sender_notice::resolve_sender(&origin(None), &cards).is_none());
    assert!(sender_notice::resolve_sender(&origin(Some("gone")), &cards).is_none());
    assert_eq!(
        sender_notice::resolve_sender(&origin(Some("gone")), &cards[..1])
            .map(|agent| agent.agent_id.as_str()),
        Some("docs-session"),
        "a name only one live card holds needs no channel"
    );
}

#[test]
fn sender_notice_prose_carries_evidence_without_a_blind_resend() {
    let q = Queue::new();
    let mut record = q.record(1).with_address(Some("@receiver#docs".into()));
    record.status = MessageStatus::TimedOut;
    record.attempts = 3;
    record.unconfirmed_sends = 2;
    record.last_sent_at = Some(Timestamp::now());
    record.text = format!("recognize\nthis {}", "x".repeat(100));
    let text = sender_notice::undelivered(&record, Some("delivery unconfirmed"));
    for evidence in [
        record.message_id.to_string(),
        "@receiver#docs".into(),
        "timed out".into(),
        "delivery unconfirmed".into(),
        "3 claims".into(),
        "2 unconfirmed writes".into(),
        record.last_sent_at.unwrap().to_string(),
        "recognize this".into(),
        "rimz agents show @receiver#docs".into(),
    ] {
        assert!(text.contains(&evidence), "missing {evidence}: {text}");
    }
    assert!(!text.contains(&"x".repeat(81)));
    assert!(!text.contains("rimz message @"));
    let queued = sender_notice::still_queued(&record, "busy in a turn", Duration::from_secs(1200));
    assert!(queued.contains("@receiver#docs"));
    assert!(queued.contains("20m"));
    assert!(queued.contains("busy in a turn"));
    assert!(queued.contains("stays queued"));
    assert!(queued.contains(&format!("rimz message cancel {}", record.message_id)));
    let notice = sender_notice::compose(
        q.workspace_id.clone(),
        &agent(),
        HarnessNotice::MessageQueued,
        queued,
    );
    assert_eq!(
        notice.sender,
        MessageSender::Harness {
            notice: HarnessNotice::MessageQueued
        }
    );
    assert_eq!(notice.body, MessageBody::Prompt);
    assert_eq!(notice.gate, DeliveryGate::Done);
}

fn register_sender(q: &Queue, ended: bool) -> AgentState {
    let kind = AgentKind::new_unchecked("claude");
    let mut observation =
        AgentLifecycleObservation::new(Some("sender-session".into()), LifecycleSignal::Registered);
    observation.agent_name = Some("sender".into());
    observation.launch.channel = Some("sender-lane".into());
    q.append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
        session_name: "session",
        agent_kind: kind.clone(),
        event_name: "test",
        observation: &observation,
        spawned_subagents: &[],
    })
    .unwrap();
    if ended {
        observation.signal = LifecycleSignal::Ended;
        q.append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "session",
            agent_kind: kind,
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    }
    q.runtime_projection(crate::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.name.as_deref() == Some("sender"))
        .unwrap()
}

fn sender() -> MessageSender {
    MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: Some("sender-session".into()),
        name: Some("sender".into()),
        profile: None,
        role: None,
        channel: None,
    }
}

fn assert_notice(q: &Queue, original: &MessageRecord, status: &str) -> MessageRecord {
    let live = q.live();
    assert_eq!(
        live.len(),
        1,
        "the closing commit must queue one sender notice"
    );
    let notice = &live[0];
    assert_eq!(notice.agent_id.as_str(), "sender-session");
    assert_eq!(notice.channel.as_deref(), Some("sender-lane"));
    assert_eq!(notice.status, MessageStatus::Queued);
    assert_eq!(
        serde_json::to_value(&notice.sender).unwrap(),
        serde_json::json!({"origin": "harness", "notice": "message_undelivered"})
    );
    assert!(notice.text.contains(original.message_id.as_str()));
    assert!(notice.text.contains(status));
    assert!(notice.text.contains("next"));
    assert!(notice.text.contains("rimz agents show"));
    assert!(!notice.text.contains("rimz message @"));
    assert!(notice.in_reply_to.is_empty());
    assert_eq!(notice.pane_id, None);
    assert!(!notice.automated);
    let events = q.events();
    let event = events.last().unwrap();
    assert_eq!(event.method, "message.queued");
    assert_eq!(
        event.params_value()["message_id"],
        notice.message_id.as_str()
    );
    assert!(!serde_json::to_string(event).unwrap().contains(&notice.text));
    notice.clone()
}

#[test]
fn every_undelivered_ending_queues_one_nonrecursive_notice() {
    for (status, word) in [
        (MessageStatus::Archived, "archived"),
        (MessageStatus::TimedOut, "timed out"),
        (MessageStatus::Abandoned, "abandoned"),
        (MessageStatus::Expired, "expired"),
    ] {
        let q = Queue::new();
        register_sender(&q, false);
        let original = q.queue_with(1, |record| record.sender = sender());
        q.settle(&original.message_id, status, Some("test reason"));
        let notice = assert_notice(&q, &original, word);
        assert!(notice.text.contains("test reason"));
        assert!(q.settle(&original.message_id, status, None).is_none());
        assert_eq!(q.live(), vec![notice.clone()]);
        q.settle(
            &notice.message_id,
            MessageStatus::Archived,
            Some("sender ended"),
        );
        assert!(
            q.live().is_empty(),
            "a harness notice cannot cause another notice"
        );
    }
}

#[test]
fn deliberate_or_delivered_endings_do_not_notify() {
    for status in [MessageStatus::Canceled, MessageStatus::Delivered] {
        let q = Queue::new();
        register_sender(&q, false);
        let original = q.queue_with(1, |record| record.sender = sender());
        q.settle(&original.message_id, status, None);
        assert!(q.live().is_empty());
    }
}

#[test]
fn a_send_error_the_senders_own_command_reports_does_not_notify() {
    let q = Queue::new();
    register_sender(&q, false);
    let queued = q.queue_with(1, |record| record.sender = sender());
    let claimed = q
        .claim_message_for_delivery(&queued.message_id, Timestamp::now())
        .unwrap()
        .expect("claim");
    let errored = q
        .record_send_error(&claimed, "agent is waiting on input in its pane", "session")
        .unwrap()
        .expect("held");
    assert_eq!(errored.status, MessageStatus::Errored);
    assert!(q.live().is_empty());
}

#[test]
fn nonagent_and_waiting_senders_do_not_notify() {
    for (sender, reply_wait) in [
        (MessageSender::Human, false),
        (MessageSender::System, false),
        (
            MessageSender::Subagent {
                kind: AgentKind::new_unchecked("claude"),
                name: "sender".into(),
            },
            false,
        ),
        (
            MessageSender::Harness {
                notice: crate::store::message::HarnessNotice::Wait,
            },
            false,
        ),
        (sender(), true),
    ] {
        let q = Queue::new();
        register_sender(&q, false);
        let original = q.queue_with(1, |record| {
            record.sender = sender;
            record.reply_wait = reply_wait;
        });
        q.settle(&original.message_id, MessageStatus::Archived, None);
        assert!(q.live().is_empty());
    }
}

#[test]
fn absent_or_ended_sender_has_nobody_to_notify() {
    for ended in [None, Some(true)] {
        let q = Queue::new();
        if let Some(ended) = ended {
            register_sender(&q, ended);
        }
        let original = q.queue_with(1, |record| record.sender = sender());
        q.settle(&original.message_id, MessageStatus::Archived, None);
        assert!(q.live().is_empty());
    }
}

#[test]
fn orphan_archival_queues_sender_notice() {
    let q = Queue::new();
    register_sender(&q, false);
    let original = q.queue_with(1, |record| record.sender = sender());
    assert_eq!(q.archive_orphan_messages("session").unwrap(), 1);
    assert!(
        assert_notice(&q, &original, "archived")
            .text
            .contains("receiver ended")
    );
}

#[test]
fn unconfirmed_send_cap_queues_sender_notice() {
    let q = Queue::new();
    register_sender(&q, false);
    let original = q.sent_with(1, |record| record.sender = sender());
    let report = q
        .reconcile_stale_messages(
            "session",
            Timestamp::now() + original.body.delivery_window() + Duration::from_secs(1),
            0,
        )
        .unwrap();
    assert_eq!(report.timed_out, 1);
    assert_notice(&q, &original, "timed out");
}

#[test]
fn queued_notice_stamp_survives_queue_and_history_codecs() {
    let q = Queue::new();
    let mut json = serde_json::to_value(q.record(1)).unwrap();
    let stamp = Timestamp::now();
    json["queued_notice_at"] = serde_json::to_value(stamp).unwrap();
    let record: MessageRecord = serde_json::from_value(json).unwrap();
    q.queue_message(&record, "session").unwrap();
    assert_eq!(
        serde_json::to_value(q.by_id(&record.message_id)).unwrap()["queued_notice_at"],
        serde_json::to_value(stamp).unwrap()
    );
    q.settle(&record.message_id, MessageStatus::Archived, None);
    assert_eq!(
        serde_json::to_value(&q.history()[0]).unwrap()["queued_notice_at"],
        serde_json::to_value(stamp).unwrap()
    );
    let requeued = MessageRecord::requeue_from(&q.history()[0]);
    assert!(
        serde_json::to_value(requeued)
            .unwrap()
            .get("queued_notice_at")
            .is_none()
    );
    assert!(
        serde_json::to_value(q.record(2))
            .unwrap()
            .get("queued_notice_at")
            .is_none()
    );
}
