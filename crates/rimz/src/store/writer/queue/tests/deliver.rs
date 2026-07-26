use super::*;

fn user_message(text: &str) -> String {
    format!("Type: USER_MESSAGE\nFrom: @user\nContent:\n{text}")
}

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
        .reconcile_stale_sent_messages("session", Timestamp::now(), 3, |_| false)
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
    assert!(sent.iter().all(|message| message.last_sent_at.is_some()));
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
            DeliveryAck::TurnStarted {
                prompt: Some(&user_message("next")),
            },
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
            DeliveryAck::Compaction,
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

        let ack = match case.confirmed {
            MessageBody::Prompt => DeliveryAck::TurnStarted { prompt: None },
            MessageBody::Command => DeliveryAck::Compaction,
        };
        let delivered = q
            .confirm_delivered_for_card(&head.kind, &head.agent_id, None, ack, "session")
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
fn correlated_ack_confirms_matching_prompt_instead_of_oldest_sent() {
    let q = Queue::new();
    let oldest = q.sent_with(1, |message| message.text = "human typed this".to_owned());
    let matching = q.sent_with(2, |message| message.text = "rimz prompt".to_owned());

    let delivered = q
        .confirm_delivered_for_card(
            &matching.kind,
            &matching.agent_id,
            None,
            DeliveryAck::TurnStarted {
                prompt: Some(&user_message("rimz prompt")),
            },
            "session",
        )
        .unwrap();

    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message_id, matching.message_id);
    assert_eq!(q.live(), vec![oldest]);
}

#[test]
fn correlated_ack_aligns_headered_and_mixed_batches() {
    for (first_sender, first_text, second_text, prompt) in [
        (
            MessageSender::Human,
            "first",
            "second",
            format!("{}\n\n{}", user_message("first"), user_message("second")),
        ),
        (
            MessageSender::Agent {
                kind: AgentKind::new_unchecked("codex"),
                name: None,
                profile: None,
                role: Some("planner".to_owned()),
                channel: None,
            },
            "first",
            "second",
            format!(
                "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nfirst\n\n{}",
                user_message("second")
            ),
        ),
        (
            MessageSender::Human,
            "first\n",
            "second",
            format!("{}\n\n{}", user_message("first\n"), user_message("second")),
        ),
        (
            MessageSender::Human,
            "first",
            "\nsecond",
            format!("{}\n\n{}", user_message("first"), user_message("\nsecond")),
        ),
        (
            MessageSender::Human,
            "\nfirst",
            "second\n",
            format!(
                "{}\n\n{}",
                user_message("\nfirst"),
                user_message("second\n")
            ),
        ),
    ] {
        let q = Queue::new();
        let first = q.queue_with(1, |message| {
            message.text = first_text.to_owned();
            message.sender = first_sender;
        });
        q.queue_with(2, |message| message.text = second_text.to_owned());
        let claimed = q
            .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
            .unwrap()
            .expect("batch claimed");
        let sent = q.record_sent_batch(&claimed, "session").unwrap();

        let delivered = q
            .confirm_delivered_for_card(
                &first.kind,
                &first.agent_id,
                None,
                DeliveryAck::TurnStarted {
                    prompt: Some(&prompt),
                },
                "session",
            )
            .unwrap();

        assert_eq!(
            delivered
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>(),
            sent.iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>()
        );
        assert!(q.live().is_empty());
    }
}

#[test]
fn correlated_ack_ignores_unmatched_reported_text() {
    let q = Queue::new();
    let sent = q.sent_with(1, |message| message.text = "rimz prompt".to_owned());

    let delivered = q
        .confirm_delivered_for_card(
            &sent.kind,
            &sent.agent_id,
            None,
            DeliveryAck::TurnStarted {
                prompt: Some("human prompt"),
            },
            "session",
        )
        .unwrap();

    assert!(delivered.is_empty());
    assert_eq!(q.live(), vec![sent]);
}

#[test]
fn correlated_ack_absorbs_queued_late_ack_but_not_a_claimed_record() {
    let q = Queue::new();
    let queued = q.sent_with(1, |message| message.text = "late prompt".to_owned());
    let claimed = q.sent_with(2, |message| message.text = "claimed prompt".to_owned());
    let reconcile_at =
        claimed.last_sent_at.expect("sent timestamp") + MessageBody::Prompt.delivery_window();
    let report = q
        .reconcile_stale_sent_messages("session", reconcile_at, 3, |_| false)
        .unwrap();
    assert_eq!(report.requeued, 2);
    q.claim_message_for_steer(&claimed.message_id, reconcile_at)
        .unwrap()
        .expect("claimed");

    let late = q
        .confirm_delivered_for_card(
            &queued.kind,
            &queued.agent_id,
            None,
            DeliveryAck::TurnStarted {
                prompt: Some(&user_message("late prompt")),
            },
            "session",
        )
        .unwrap();
    let claimed_ack = q
        .confirm_delivered_for_card(
            &claimed.kind,
            &claimed.agent_id,
            None,
            DeliveryAck::TurnStarted {
                prompt: Some(&user_message("claimed prompt")),
            },
            "session",
        )
        .unwrap();

    assert_eq!(late.len(), 1);
    assert_eq!(late[0].message_id, queued.message_id);
    assert!(claimed_ack.is_empty());
    assert_eq!(q.live()[0].message_id, claimed.message_id);
    assert_eq!(q.live()[0].status, MessageStatus::Claimed);
}

#[test]
fn stale_sent_message_requeues_before_attempt_cap() {
    let q = Queue::new();
    let sent = q.sent_with(1, |message| {
        message.batch_id = Some(message.message_id.clone());
    });
    let last_sent_at = sent.last_sent_at.expect("last sent");

    let report = q
        .reconcile_stale_sent_messages(
            "session",
            last_sent_at + sent.body.delivery_window(),
            3,
            |_| false,
        )
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
    assert_eq!(messages[0].last_sent_at, Some(last_sent_at));
    assert_eq!(
        messages[0].last_error.as_deref(),
        Some("delivery unconfirmed; re-queued")
    );
    assert_eq!(q.reason("message.queued"), "reconcile");
}

#[test]
fn stale_command_times_out_without_resend_regardless_of_counter() {
    let q = Queue::new();
    let sent = q.sent_with(1, |message| {
        message.body = MessageBody::Command;
        message.text = "/compact".to_owned();
        message.unconfirmed_sends = 99;
    });
    let now = sent.last_sent_at.expect("last sent") + sent.body.delivery_window();

    let report = q
        .reconcile_stale_sent_messages("session", now, 3, |_| false)
        .unwrap();

    assert_eq!(
        report,
        ReconcileReport {
            requeued: 0,
            timed_out: 1
        }
    );
    assert!(q.live().is_empty());
    let history = q.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, MessageStatus::TimedOut);
    assert_eq!(history[0].unconfirmed_sends, 99);
    assert_eq!(
        history[0].last_error.as_deref(),
        Some("delivery unconfirmed; command not resent")
    );
}

#[test]
fn stale_sent_message_is_deferred_while_receiver_compacts() {
    let q = Queue::new();
    let sent = q.sent(1);
    let window = sent.body.delivery_window();
    let now = sent.last_sent_at.expect("last sent") + window;

    let report = q
        .reconcile_stale_sent_messages("session", now, 3, |_| true)
        .unwrap();

    assert_eq!(report, ReconcileReport::default());
    let messages = q.live();
    assert_eq!(messages[0].status, MessageStatus::Sent);
    assert_eq!(messages[0].unconfirmed_sends, 0);
    assert_eq!(messages[0].retry_after, Some(now + window));
    assert_eq!(messages[0].wake_deadline(now), Some(now + window));
}

#[test]
fn stale_sent_message_times_out_at_attempt_cap() {
    let q = Queue::new();
    let sent = q.sent_with(1, |message| message.unconfirmed_sends = 3);
    let now = sent.last_sent_at.expect("last sent") + sent.body.delivery_window();

    let report = q
        .reconcile_stale_sent_messages("session", now, 3, |_| false)
        .unwrap();

    assert_eq!(report.requeued, 0);
    assert_eq!(report.timed_out, 1);
    assert!(q.live().is_empty());
    assert_eq!(q.reason("message.timed_out"), "reconcile");
}

#[test]
fn stale_sent_reconcile_preserves_cross_message_event_order() {
    let q = Queue::new();
    let first = q.sent_with(1, |message| message.unconfirmed_sends = 3);
    let second = q.sent(2);
    let now = [first, second]
        .into_iter()
        .filter_map(|message| message.sent_reconcile_deadline())
        .max()
        .expect("deadline");

    q.reconcile_stale_sent_messages("session", now, 3, |_| false)
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
        .reconcile_stale_sent_messages("session", Timestamp::now(), 3, |_| false)
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

    let wake = q.earliest_message_wake(sent.updated_at).unwrap();

    assert_eq!(wake, sent.sent_reconcile_deadline());
}
