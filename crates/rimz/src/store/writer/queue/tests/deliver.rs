use super::*;

#[test]
fn defer_message_wake_sets_retry_after_only_for_queued_messages() {
    let q = Queue::new();
    let queued = q.queue(1);
    let until = Timestamp::now() + Duration::from_secs(30);

    q.defer_message_wake(&queued.message_id, until).unwrap();

    assert_eq!(
        q.list_pending_messages().unwrap()[0].retry_after,
        Some(until)
    );

    let claimed = q
        .claim_message_for_delivery(&queued.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    assert_eq!(claimed.retry_after, None);
    let sent = q
        .record_sent_message(&claimed, "session")
        .unwrap()
        .expect("sent");
    q.defer_message_wake(&sent.message_id, until + Duration::from_secs(30))
        .unwrap();

    let messages = q.live();
    assert_eq!(messages[0].status, MessageStatus::Sent);
    assert_eq!(messages[0].retry_after, None);

    q.defer_message_wake(&message_id(999), until).unwrap();
}

#[test]
fn no_op_queue_transaction_changes_no_durable_surface() {
    let q = Queue::new();
    q.queue(1);
    let queue_path = q.inner.paths.messages_dir.join("messages.jsonl");
    let queue_before = std::fs::read(&queue_path).unwrap();
    let events_before = std::fs::read(&q.inner.paths.events_log).unwrap();
    let _ = std::fs::remove_file(&q.inner.paths.latest_snapshot);

    let report = q
        .reconcile_stale_sent_messages(
            "session",
            Timestamp::now(),
            Duration::from_secs(30),
            3,
            |_| false,
        )
        .unwrap();

    assert_eq!(report, ReconcileReport::default());
    assert_eq!(std::fs::read(queue_path).unwrap(), queue_before);
    assert!(q.history().is_empty());
    assert_eq!(
        std::fs::read(&q.inner.paths.events_log).unwrap(),
        events_before
    );
    assert!(!q.inner.paths.latest_snapshot.exists());
}

#[test]
fn sent_batch_persists_identity_and_events_in_source_order() {
    let q = Queue::new();
    let first = q.queue(1);
    let second = q.queue(2);
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");
    let claimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
        .unwrap()
        .expect("batch claimed")
        .into_iter()
        .map(|message| message.with_pane_id(pane_id.clone()))
        .collect::<Vec<_>>();

    let sent = q.record_sent_batch(&claimed, "session").unwrap();

    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter()
            .all(|message| message.status == MessageStatus::Sent)
    );
    assert!(
        sent.iter()
            .all(|message| message.pane_id.as_ref() == Some(&pane_id))
    );
    assert!(
        sent.iter()
            .all(|message| message.batch_id.as_ref() == Some(&first.message_id))
    );
    // The batch identity reaches the durable record, not just the return value.
    assert!(
        q.live()
            .iter()
            .all(|message| message.batch_id.as_ref() == Some(&first.message_id))
    );
    let sent_event_ids = q
        .events()
        .into_iter()
        .filter(|event| event.method == "message.sent")
        .map(|event| {
            event.params_value()["message_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sent_event_ids,
        vec![first.message_id.to_string(), second.message_id.to_string()]
    );
}

#[test]
fn record_sent_then_turn_start_confirms_delivery() {
    let q = Queue::new();

    let sent = q.sent(1);
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.pane_id.as_ref().map(PaneId::as_str), Some("tmux:%1"));
    assert!(q.list_pending_messages().unwrap().is_empty());

    let delivered = q
        .confirm_delivered_for_card(
            &sent.kind,
            &sent.agent_id,
            None,
            MessageBody::Prompt,
            "session",
        )
        .unwrap()
        .into_iter()
        .next()
        .expect("delivered");
    assert_eq!(delivered.status, MessageStatus::Delivered);
    assert!(delivered.delivered_at.is_some());
    assert!(q.live().is_empty());
    assert_eq!(q.count("message.delivered"), 1);
}

#[test]
fn idle_compact_command_delivers_at_boundary_and_stamps_the_rollup() {
    let q = Queue::new();
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::Registered,
    );
    q.append_event(&EventEnvelope::agent_lifecycle(
        q.workspace_id.clone(),
        "session",
        "claude",
        "SessionStart",
        &observation,
    ))
    .unwrap();
    let command = q.queue_with(1, |message| {
        message.text = "/compact".to_owned();
        message.body = MessageBody::Command;
        message.compacted_context_tokens = Some(80_000);
    });
    let claimed = q
        .claim_message_for_delivery(&command.message_id, Timestamp::now())
        .unwrap()
        .expect("idle boundary claims command");
    let sent = q
        .record_sent_message(&claimed, "session")
        .unwrap()
        .expect("command sent");
    let delivered = q
        .confirm_delivered_for_card(
            &sent.kind,
            &sent.agent_id,
            sent.agent_name.as_deref(),
            MessageBody::Command,
            "session",
        )
        .unwrap();
    assert_eq!(delivered.len(), 1);

    let (_, agents, _) = crate::store::snapshot::catch_up_rollup(&q.inner.paths).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].last_compact_command_tokens, Some(80_000));
}

#[test]
fn confirm_delivered_for_card_selects_oldest_matching_batch() {
    struct Case {
        name: &'static str,
        /// `(id, batch_id, body)`, recorded as sent in this order.
        sent: &'static [(u64, Option<u64>, MessageBody)],
        confirmed: MessageBody,
        delivered: &'static [u64],
        survivors: &'static [u64],
    }

    let cases = [
        Case {
            name: "no batch delivers only the oldest",
            sent: &[
                (1, None, MessageBody::Prompt),
                (2, None, MessageBody::Prompt),
            ],
            confirmed: MessageBody::Prompt,
            delivered: &[1],
            survivors: &[2],
        },
        Case {
            name: "a shared batch delivers every member",
            sent: &[
                (1, Some(1), MessageBody::Prompt),
                (2, Some(1), MessageBody::Prompt),
                (3, Some(99), MessageBody::Prompt),
            ],
            confirmed: MessageBody::Prompt,
            delivered: &[1, 2],
            survivors: &[3],
        },
        Case {
            name: "a mismatched body is skipped",
            sent: &[
                (1, None, MessageBody::Prompt),
                (2, None, MessageBody::Command),
            ],
            confirmed: MessageBody::Command,
            delivered: &[2],
            survivors: &[1],
        },
    ];

    for case in cases {
        let q = Queue::new();
        let mut head = None;
        for (id, batch_id, body) in case.sent {
            let sent = q.sent_with(*id, |record| {
                record.body = *body;
                record.batch_id = batch_id.map(message_id);
            });
            head.get_or_insert(sent);
        }
        let head = head.expect("at least one sent record");
        let ids = |ids: &[u64]| ids.iter().copied().map(message_id).collect::<Vec<_>>();

        let delivered = q
            .confirm_delivered_for_card(&head.kind, &head.agent_id, None, case.confirmed, "session")
            .unwrap();

        assert_eq!(
            delivered
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>(),
            ids(case.delivered),
            "{}",
            case.name
        );
        assert!(
            delivered
                .iter()
                .all(|message| message.status == MessageStatus::Delivered
                    && message.delivered_at.is_some()),
            "{}",
            case.name
        );
        let live = q.live();
        assert_eq!(
            live.iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>(),
            ids(case.survivors),
            "{}",
            case.name
        );
        assert!(
            live.iter()
                .all(|message| message.status == MessageStatus::Sent),
            "{}",
            case.name
        );
        assert_eq!(
            q.count("message.delivered"),
            case.delivered.len(),
            "{}",
            case.name
        );
    }
}

#[test]
fn stale_sent_message_requeues_before_attempt_cap() {
    let q = Queue::new();
    q.sent_with(1, |message| {
        message.batch_id = Some(message.message_id.clone());
    });

    let report = q
        .reconcile_stale_sent_messages("session", Timestamp::now(), Duration::ZERO, 3, |_| false)
        .unwrap();

    assert_eq!(report.requeued, 1);
    assert_eq!(report.timed_out, 0);
    let messages = q.live();
    assert_eq!(messages[0].status, MessageStatus::Queued);
    assert_eq!(messages[0].pane_id, None);
    assert_eq!(messages[0].batch_id, None);
    assert_eq!(messages[0].attempts, 0);
    // Pinned by 94f521220: unconfirmed sends count separately from attempts.
    assert_eq!(messages[0].unconfirmed_sends, 1);
    assert_eq!(messages[0].last_attempt_at, None);
    assert_eq!(
        messages[0].last_error.as_deref(),
        Some("delivery unconfirmed; re-queued")
    );
    assert_eq!(q.reason("message.queued"), "reconcile");
}

#[test]
fn stale_sent_message_is_deferred_while_receiver_compacts() {
    let q = Queue::new();
    q.sent(1);
    let now = Timestamp::now() + Duration::from_secs(60);
    let window = Duration::from_secs(30);

    let report = q
        .reconcile_stale_sent_messages("session", now, window, 3, |_| true)
        .unwrap();

    assert_eq!(report, ReconcileReport::default());
    let messages = q.live();
    assert_eq!(messages[0].status, MessageStatus::Sent);
    assert_eq!(messages[0].unconfirmed_sends, 0);
    assert_eq!(messages[0].retry_after, Some(now + window));
    assert_eq!(messages[0].wake_deadline(now, window), Some(now + window));
}

#[test]
fn stale_sent_message_times_out_at_attempt_cap() {
    let q = Queue::new();
    q.sent_with(1, |message| message.unconfirmed_sends = 3);

    let report = q
        .reconcile_stale_sent_messages("session", Timestamp::now(), Duration::ZERO, 3, |_| false)
        .unwrap();

    assert_eq!(report.requeued, 0);
    assert_eq!(report.timed_out, 1);
    assert!(q.live().is_empty());
    assert_eq!(q.reason("message.timed_out"), "reconcile");
}

#[test]
fn stale_sent_reconcile_preserves_cross_message_event_order() {
    let q = Queue::new();
    q.sent_with(1, |message| message.unconfirmed_sends = 3);
    q.sent(2);

    q.reconcile_stale_sent_messages("session", Timestamp::now(), Duration::ZERO, 3, |_| false)
        .unwrap();

    let methods = q.methods();
    assert_eq!(
        methods.iter().rev().take(2).collect::<Vec<_>>(),
        ["message.queued", "message.timed_out"]
    );
}

#[test]
fn fresh_sent_message_waits_for_reconcile_deadline() {
    let q = Queue::new();
    q.sent(1);

    let report = q
        .reconcile_stale_sent_messages(
            "session",
            Timestamp::now(),
            Duration::from_secs(60),
            3,
            |_| false,
        )
        .unwrap();

    assert_eq!(report, ReconcileReport::default());
    assert_eq!(q.live()[0].status, MessageStatus::Sent);
}

#[test]
fn earliest_message_wake_includes_sent_reconcile_deadline() {
    let q = Queue::new();
    let sent = q.sent(1);
    q.queue_with(2, |scheduled| {
        scheduled.not_before = Some(sent.updated_at + Duration::from_secs(120));
    });

    let wake = q
        .earliest_message_wake(sent.updated_at, Duration::from_secs(30))
        .unwrap();

    assert_eq!(wake, Some(sent.updated_at + Duration::from_secs(30)));
}
