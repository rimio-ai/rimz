use super::*;
use crate::store::message::Premise;

const PROMPTED: &str = "agent was prompted after this message was queued";

fn prompted_since(copy: Option<Timestamp>) -> Vec<Premise> {
    vec![Premise::NotPromptedSince {
        last_prompt_at: copy,
    }]
}

fn agent_sender() -> MessageSender {
    MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: Some("sender-session".into()),
        name: Some("sender".into()),
        profile: None,
        role: None,
        channel: None,
    }
}

fn turn_start(text: &str) -> String {
    format!("Type: USER_MESSAGE\nFrom: @user\nContent:\n{text}")
}

/// The card row after a real prompt at `at`, as the hook reads it.
fn prompted_row(at: Timestamp) -> AgentState {
    let mut row = agent();
    row.last_prompt_at = Some(at);
    row
}

#[test]
fn turn_start_selects_acknowledgement_first_then_withdraws_broken_queued_records() {
    let q = Queue::new();
    let copy = Some(Timestamp::now() - Duration::from_secs(60));
    let sent = q.sent_with(1, |record| {
        record.text = "requeued".to_owned();
        record.premises = prompted_since(copy);
    });
    let report = q
        .reconcile_stale_messages(
            "session",
            sent.last_sent_at.unwrap() + sent.body.delivery_window(),
            3,
        )
        .unwrap();
    assert_eq!(report.requeued, 1);
    assert!(q.by_id(&sent.message_id).awaiting_late_ack());
    let broken = q.queue_with(2, |record| {
        record.premises = prompted_since(copy);
        record.sender = agent_sender();
    });
    let prompted_at = Timestamp::now();
    let held = q.queue_with(3, |record| {
        record.premises = prompted_since(Some(prompted_at));
    });
    let other_card = q.queue_with(4, |record| {
        record.agent_id = "other".into();
        record.premises = prompted_since(copy);
    });
    let plain = q.queue(5);
    let row = prompted_row(prompted_at);

    let delivered = q
        .confirm_delivered_for_card_with(
            AgentCardRef::new(&sent.kind, &sent.agent_id, None),
            DeliveryAck::TurnStarted {
                prompt: Some(&turn_start("requeued")),
            },
            Some(&row),
            "session",
            |_, _| {},
        )
        .unwrap();

    assert_eq!(
        delivered
            .iter()
            .map(|record| (&record.message_id, record.status))
            .collect::<Vec<_>>(),
        [(&sent.message_id, MessageStatus::Delivered)]
    );
    assert_eq!(
        q.live()
            .iter()
            .map(|record| &record.message_id)
            .collect::<Vec<_>>(),
        [&held.message_id, &other_card.message_id, &plain.message_id]
    );
    let withdrawn = q
        .history()
        .into_iter()
        .find(|record| record.message_id == broken.message_id)
        .expect("withdrawn record in history");
    assert_eq!(withdrawn.status, MessageStatus::Canceled);
    assert_eq!(withdrawn.last_error.as_deref(), Some(PROMPTED));
    assert_eq!(q.reason("message.canceled"), PROMPTED);
}

#[test]
fn turn_start_withdraws_even_when_it_acknowledges_nothing_and_only_for_a_later_prompt() {
    let q = Queue::new();
    let copy = Timestamp::now() - Duration::from_secs(60);
    let queued = q.queue_with(1, |record| record.premises = prompted_since(Some(copy)));
    let card = AgentCardRef::new(&queued.kind, &queued.agent_id, None);
    let confirm = |row: Option<&AgentState>| {
        q.confirm_delivered_for_card_with(
            card,
            DeliveryAck::TurnStarted {
                prompt: Some(&turn_start("unrelated")),
            },
            row,
            "session",
            |_, _| {},
        )
        .unwrap()
    };

    let keepalive_only = prompted_row(copy);
    assert!(confirm(Some(&keepalive_only)).is_empty());
    assert!(confirm(None).is_empty());
    assert_eq!(q.live().len(), 1);
    assert!(confirm(Some(&prompted_row(Timestamp::now()))).is_empty());
    assert!(q.live().is_empty());
    assert_eq!(q.history()[0].last_error.as_deref(), Some(PROMPTED));
}

#[test]
fn withdrawal_cancels_open_records_with_their_reason_and_no_sender_notice() {
    let q = Queue::new();
    let queued = q.queue_with(1, |record| record.sender = agent_sender());
    let claimed = q.queue_with(2, |record| {
        record.agent_id = "claimed-card".into();
        record.sender = agent_sender();
    });
    q.claim_message_for_delivery(&claimed.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    let sent = q.sent_with(3, |record| record.sender = agent_sender());
    let kept = q.queue_with(4, |record| record.sender = agent_sender());

    let withdrawn = q
        .withdraw_messages("session", |record| {
            (record.message_id != kept.message_id).then(|| format!("gone {}", record.message_id))
        })
        .unwrap();

    assert_eq!(
        withdrawn
            .iter()
            .map(|record| (&record.message_id, record.status, record.last_error.clone()))
            .collect::<Vec<_>>(),
        [&queued.message_id, &claimed.message_id].map(|id| (
            id,
            MessageStatus::Canceled,
            Some(format!("gone {id}"))
        ))
    );
    assert_eq!(
        q.live()
            .iter()
            .map(|record| &record.message_id)
            .collect::<Vec<_>>(),
        [&sent.message_id, &kept.message_id]
    );
}

#[test]
fn lifespan_arms_the_wake_stamp_and_ends_through_reconcile_without_a_notice() {
    let q = Queue::new();
    let now = Timestamp::now();
    let until = now + Duration::from_secs(120);
    let lifespan = vec![Premise::Lifespan { until }];
    let scheduled = q.queue_with(1, |record| {
        record.not_before = Some(now + Duration::from_secs(3_600));
        record.premises = lifespan.clone();
        record.sender = agent_sender();
    });
    let resume = q.queue_with(2, |record| {
        record.agent_id = "other".into();
        record.gate = DeliveryGate::Resume;
        record.premises = lifespan.clone();
    });
    let stamp: Option<Timestamp> =
        serde_json::from_slice(&std::fs::read(q.runtime_paths().message_wake_path()).unwrap())
            .unwrap();
    assert_eq!(stamp, Some(until));
    let resume_record = q.by_id(&resume.message_id);
    assert!(resume_record.is_deliverable(now));
    assert!(!resume_record.is_deliverable(until));

    q.reconcile_stale_messages("session", until, 3).unwrap();

    assert!(q.live().is_empty());
    let ended = q.history();
    assert_eq!(
        ended
            .iter()
            .map(|record| (
                &record.message_id,
                record.status,
                record.last_error.as_deref()
            ))
            .collect::<Vec<_>>(),
        [&scheduled.message_id, &resume.message_id].map(|id| (
            id,
            MessageStatus::Canceled,
            Some("lifespan ended before delivery")
        ))
    );
}

#[test]
fn every_premise_withdrawal_keeps_its_reason_on_the_record() {
    use crate::store::message::{HarnessNotice, SoftCompactStep};

    let q = Queue::new();
    let premised = q.queue_with(1, |record| record.premises = prompted_since(None));
    let about_run = q.queue_with(2, |record| {
        record.about_run = Some(crate::ids::RunId::new())
    });
    let soft = q.queue_with(3, |record| {
        record.soft_compact = Some(SoftCompactStep::Continue);
    });
    let digest = q.queue_with(4, |record| {
        record.sender = MessageSender::Harness {
            notice: HarnessNotice::SubagentReport,
        };
    });
    let plain = q.queue(5);
    for record in [&premised, &about_run, &soft, &digest, &plain] {
        q.settle(&record.message_id, MessageStatus::Canceled, Some("broke"));
    }

    assert_eq!(
        q.history()
            .iter()
            .map(|record| record.last_error.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("broke"),
            Some("broke"),
            Some("broke"),
            Some("broke"),
            None
        ]
    );
}
