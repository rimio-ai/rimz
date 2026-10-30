use super::*;

fn wrap_up(q: &Queue) -> MessageRecord {
    let mut encoded = serde_json::to_value(
        q.record(1)
            .with_sender(MessageSender::System)
            .with_automated(true)
            .with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1")),
    )
    .unwrap();
    encoded["soft_compact"] = serde_json::json!("wrap_up");
    encoded["gate"] = serde_json::json!("any");
    serde_json::from_value(encoded).unwrap()
}

fn sent_wrap_up(q: &Queue) -> MessageRecord {
    let record = wrap_up(q);
    q.queue_message(&record, "session").unwrap();
    q.record_sent_message(&record, "session").unwrap().unwrap()
}

fn observe(q: &Queue, signal: LifecycleSignal, at: Timestamp) {
    let mut event = EventEnvelope::agent_lifecycle(
        q.workspace_id.clone(),
        "session",
        "claude",
        "test.lifecycle",
        &AgentLifecycleObservation::new(Some("sess-1".into()), signal),
    );
    event.timestamp = at;
    q.append_event(&event).unwrap();
}

#[test]
fn boundary_claim_skips_wrap_up_but_steer_can_claim_it() {
    let q = Queue::new();
    let wrap_up = wrap_up(&q);
    q.queue_message(&wrap_up, "session").unwrap();
    let next = q.queue(2);
    let now = Timestamp::now();
    let boundary = q
        .claim_delivery_batch(&wrap_up.message_id, AgentStatus::Idle, now)
        .unwrap();
    let next_batch = q
        .claim_delivery_batch(&next.message_id, AgentStatus::Idle, now)
        .unwrap();
    let steer = q.claim_message_for_steer(&wrap_up.message_id, now).unwrap();
    assert_eq!(
        (
            boundary.map(|batch| batch
                .into_iter()
                .map(|message| message.message_id)
                .collect::<Vec<_>>()),
            next_batch.map(|batch| batch
                .into_iter()
                .map(|message| message.message_id)
                .collect::<Vec<_>>()),
            steer.map(|message| message.message_id),
        ),
        (None, Some(vec![next.message_id]), Some(wrap_up.message_id))
    );
}

#[test]
fn reconcile_preserves_wrap_up_senders_until_the_claim_ttl() {
    let outcomes = [false, true].map(|claimed| {
        let q = Queue::new();
        let record = wrap_up(&q);
        q.queue_message(&record, "session").unwrap();
        if claimed {
            q.claim_message_for_steer(
                &record.message_id,
                record.updated_at + Duration::from_secs(10),
            )
            .unwrap()
            .unwrap();
        }
        let active = q.by_id(&record.message_id);
        let active_at = active.last_attempt_at.unwrap_or(active.updated_at);
        let deadline = active_at + CLAIM_TTL;
        for now in [active_at, deadline - Duration::from_nanos(1)] {
            assert_eq!(
                q.reconcile_stale_messages("session", now, 3).unwrap(),
                ReconcileReport::default()
            );
            assert_eq!(q.live(), vec![active.clone()], "live sender: {claimed}");
            assert!(q.history().is_empty());
        }
        let report = q.reconcile_stale_messages("session", deadline, 3).unwrap();
        let history = q.history();
        (
            q.live().len(),
            history.first().map(|message| message.status),
            q.count("message.canceled"),
            report.requeued,
        )
    });
    assert_eq!(outcomes, [(0, Some(MessageStatus::Canceled), 1, 0); 2]);
}

#[test]
fn reconcile_settles_sent_wrap_up_from_turn_end_without_a_resend() {
    let outcomes = [Some(-1), Some(0), Some(1), None].map(|ended_after_send| {
        let q = Queue::new();
        let sent = sent_wrap_up(&q);
        let sent_at = sent.last_sent_at.unwrap();
        if let Some(offset) = ended_after_send {
            observe(
                &q,
                LifecycleSignal::Registered,
                sent_at - Duration::from_secs(3),
            );
            observe(
                &q,
                LifecycleSignal::TurnStarted { turn_id: None },
                sent_at - Duration::from_secs(2),
            );
            observe(
                &q,
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
                sent_at + jiff::SignedDuration::from_secs(offset),
            );
        }
        let report = q
            .reconcile_stale_messages("session", sent_at + sent.body.delivery_window(), 3)
            .unwrap();
        let history = q.history();
        (
            q.live().len(),
            history.first().map(|message| message.status),
            history.first().map(|message| message.unconfirmed_sends),
            report.requeued,
            report.timed_out,
            q.count("message.queued"),
        )
    });
    assert_eq!(
        outcomes,
        [
            (0, Some(MessageStatus::TimedOut), Some(0), 0, 1, 1),
            (0, Some(MessageStatus::Delivered), Some(0), 0, 0, 1),
            (0, Some(MessageStatus::Delivered), Some(0), 0, 0, 1),
            (0, Some(MessageStatus::TimedOut), Some(0), 0, 1, 1),
        ]
    );
}

#[test]
fn reconcile_holds_wrap_up_silently_until_the_open_turn_ends() {
    let q = Queue::new();
    let sent = sent_wrap_up(&q);
    let sent_at = sent.last_sent_at.unwrap();
    observe(
        &q,
        LifecycleSignal::Registered,
        sent_at - Duration::from_secs(2),
    );
    observe(
        &q,
        LifecycleSignal::TurnStarted { turn_id: None },
        sent_at - Duration::from_secs(1),
    );
    let methods = q.methods();
    let deadline = sent_at + sent.body.delivery_window();
    let report = q.reconcile_stale_messages("session", deadline, 0).unwrap();
    let held = q.live().first().map(|held| {
        (
            held.status,
            held.unconfirmed_sends,
            held.attempts,
            held.last_error.clone(),
            held.retry_after,
        )
    });
    assert_eq!(
        (report, held, q.methods()),
        (
            ReconcileReport::default(),
            Some((
                MessageStatus::Sent,
                0,
                sent.attempts,
                None,
                Some(deadline + sent.body.delivery_window())
            )),
            methods
        )
    );
    observe(
        &q,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
        deadline + Duration::from_nanos(1),
    );
    q.reconcile_stale_messages("session", deadline + sent.body.delivery_window(), 0)
        .unwrap();
    assert_eq!(q.history()[0].status, MessageStatus::Delivered);
    assert!(q.live().is_empty());
}

#[test]
fn acknowledgement_confirms_a_tagged_wrap_up() {
    let q = Queue::new();
    let sent = sent_wrap_up(&q);
    let delivered = q
        .confirm_delivered_for_card(
            &sent.kind,
            &sent.agent_id,
            None,
            DeliveryAck::TurnStarted {
                prompt: Some(&sent.text),
            },
            "session",
        )
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].status, MessageStatus::Delivered);
    assert_eq!(
        serde_json::to_value(&delivered[0]).unwrap()["soft_compact"],
        "wrap_up"
    );
    assert_eq!(q.count("message.delivered"), 1);
}
