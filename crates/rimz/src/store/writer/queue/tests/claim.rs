use super::*;

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
fn claim_moves_message_out_of_pending_until_send_failure_requeues() {
    let q = Queue::new();
    let message = q.queue(1);

    let claimed = q
        .claim_message_for_delivery(&message.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    assert_eq!(claimed.status, MessageStatus::Claimed);
    assert_eq!(claimed.attempts, 1);
    assert!(q.list_pending_messages().unwrap().is_empty());

    q.record_message_delivery_failure(&message.message_id, "pane missing", "session")
        .unwrap();
    let pending = q.list_pending_messages().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert_eq!(pending[0].last_error.as_deref(), Some("pane missing"));
}

#[test]
fn steer_claim_skips_fifo_and_schedule_but_keeps_claim_ttl() {
    let q = Queue::new();
    q.queue(1);
    let second = q.queue(2);

    assert!(
        q.claim_message_for_delivery(&second.message_id, Timestamp::now())
            .unwrap()
            .is_none(),
        "delivery claims only the FIFO head"
    );
    let claimed = q
        .claim_message_for_steer(&second.message_id, Timestamp::now())
        .unwrap()
        .expect("steer claims non-head");
    assert_eq!(claimed.message_id, second.message_id);
    q.record_message_delivery_failure(&second.message_id, "pane missing", "session")
        .unwrap();
    assert!(
        q.claim_message_for_steer(&second.message_id, Timestamp::now())
            .unwrap()
            .is_none(),
        "fresh failed claim keeps the TTL guard"
    );

    let scheduled_q = Queue::new();
    let scheduled = scheduled_q
        .record(1)
        .with_not_before(Some(Timestamp::now() + jiff::SignedDuration::from_secs(60)));
    scheduled_q.queue_message(&scheduled, "session").unwrap();
    assert!(
        scheduled_q
            .claim_message_for_steer(&scheduled.message_id, Timestamp::now())
            .unwrap()
            .is_some(),
        "steer claims scheduled messages"
    );
}

#[test]
fn fifth_send_failure_abandons_message() {
    let q = Queue::new();
    let message = q.queue(1);

    for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
        let claimed = q
            .claim_message_for_delivery(
                &message.message_id,
                Timestamp::now() + jiff::SignedDuration::from_secs(i64::from(attempt) * 20),
            )
            .unwrap()
            .expect("claimed");
        assert_eq!(claimed.attempts, attempt);
        q.record_message_delivery_failure(&message.message_id, "pane missing", "session")
            .unwrap();
    }

    assert!(q.list_pending_messages().unwrap().is_empty());
    assert!(q.live().is_empty());
    assert_eq!(q.count("message.abandoned"), 1);
}

#[test]
fn older_claim_blocks_boundary_head_even_after_ttl() {
    let q = Queue::new();
    let first = q.queue(1);
    let second = q.queue(2);
    let now = Timestamp::now();
    q.claim_message_for_steer(&first.message_id, now)
        .unwrap()
        .expect("first claimed");

    assert!(
        q.claim_delivery_batch(&second.message_id, AgentStatus::Idle, now + CLAIM_TTL)
            .unwrap()
            .is_none()
    );
    assert_eq!(q.by_id(&first.message_id).attempts, 1);
}

#[test]
fn boundary_batch_claims_maximal_compatible_fifo_prefix() {
    let q = Queue::new();
    let first = q.record(1).with_channel(Some("same".to_owned()));
    let second = q.record(2).with_channel(Some("same".to_owned()));
    let barrier = q.record(3).with_channel(Some("other".to_owned()));
    let after_barrier = q.record(4).with_channel(Some("same".to_owned()));
    for message in [&first, &second, &barrier, &after_barrier] {
        q.queue_message(message, "session").unwrap();
    }

    let claimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
        .unwrap()
        .expect("batch claimed");

    assert_eq!(
        claimed
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<Vec<_>>(),
        vec![first.message_id.clone(), second.message_id.clone()]
    );
    assert!(claimed.iter().all(|message| message.attempts == 1));
    assert!(
        claimed
            .iter()
            .all(|message| message.batch_id.as_ref() == Some(&first.message_id))
    );
    let live = q.live();
    assert_eq!(live[0].status, MessageStatus::Claimed);
    assert_eq!(live[1].status, MessageStatus::Claimed);
    assert_eq!(live[0].batch_id, None);
    assert_eq!(live[1].batch_id, None);
    assert_eq!(live[2].status, MessageStatus::Queued);
    assert_eq!(live[3].status, MessageStatus::Queued);
}

#[test]
fn boundary_batch_stops_at_unexpired_claimed_tail() {
    let q = Queue::new();
    let first = q.queue(1);
    let claimed_tail = q.queue(2);
    let later = q.queue(3);
    let now = Timestamp::now();
    q.claim_message_for_steer(&claimed_tail.message_id, now)
        .unwrap()
        .expect("tail claimed first");

    let claimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, now)
        .unwrap()
        .expect("head claimed");

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].message_id, first.message_id);
    assert_eq!(q.by_id(&claimed_tail.message_id).attempts, 1);
    assert_eq!(q.by_id(&later.message_id).status, MessageStatus::Queued);
}

#[test]
fn boundary_batch_reclaims_expired_compatible_tail() {
    let q = Queue::new();
    let first = q.queue(1);
    let claimed_tail = q.queue(2);
    let later = q.queue(3);
    let now = Timestamp::now();
    q.claim_message_for_steer(&claimed_tail.message_id, now)
        .unwrap()
        .expect("tail claimed first");

    let claimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, now + CLAIM_TTL)
        .unwrap()
        .expect("batch claimed after TTL");

    assert_eq!(
        claimed
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<Vec<_>>(),
        vec![first.message_id, claimed_tail.message_id, later.message_id]
    );
    assert_eq!(claimed[1].attempts, 2);
}

#[test]
fn release_batch_resets_every_claim_field() {
    let q = Queue::new();
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");
    let retry_after = Timestamp::now() + Duration::from_secs(60);
    let mut first = q
        .record(1)
        .with_pane_id(pane_id.clone())
        .with_auto_compact(Some(AutoCompact::Percent(70)));
    let mut second = q
        .record(2)
        .with_pane_id(pane_id)
        .with_auto_compact(Some(AutoCompact::Percent(70)));
    first.batch_id = Some(first.message_id.clone());
    second.batch_id = Some(first.message_id.clone());
    first.retry_after = Some(retry_after);
    second.retry_after = Some(retry_after);
    for record in [&first, &second] {
        q.queue_message(record, "session").unwrap();
    }
    let claimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
        .unwrap()
        .expect("batch claimed");
    let ids = claimed
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<Vec<_>>();

    let released = q
        .release_message_claims(&ids, "waiting for compaction", "session")
        .unwrap();

    assert_eq!(released.len(), 2);
    for message in released {
        assert_eq!(message.status, MessageStatus::Queued);
        assert_eq!(message.attempts, 0);
        assert_eq!(message.last_attempt_at, None);
        assert_eq!(message.pane_id, None);
        assert_eq!(message.batch_id, None);
        assert_eq!(message.retry_after, None);
        // Pinned by 2c95a54df: a released claim clears auto_compact so the
        // fresh-window delivery does not re-fire a compact that already ran.
        assert_eq!(message.auto_compact, None);
        assert_eq!(
            message.last_error.as_deref(),
            Some("waiting for compaction")
        );
    }
}

#[test]
fn batch_failure_preserves_sent_and_requeues_or_abandons_the_rest() {
    let q = Queue::new();
    let sent = q.queue(1);
    let requeued = q.queue(2);
    let mut abandoned = q.record(3);
    abandoned.attempts = MAX_DELIVERY_ATTEMPTS - 1;
    q.queue_message(&abandoned, "session").unwrap();
    let claimed = q
        .claim_delivery_batch(&sent.message_id, AgentStatus::Idle, Timestamp::now())
        .unwrap()
        .expect("batch claimed");
    q.record_sent_batch(std::slice::from_ref(&claimed[0]), "session")
        .unwrap();
    let ids = claimed
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<Vec<_>>();

    let result = q
        .record_message_delivery_failures(
            &ids,
            None,
            DeliveryFailureDisposition::Retry,
            "pane missing",
            "session",
        )
        .unwrap();

    assert!(result.head_found);
    assert!(result.head_sent);
    assert_eq!(q.by_id(&sent.message_id).status, MessageStatus::Sent);
    let requeued = q.by_id(&requeued.message_id);
    assert_eq!(requeued.status, MessageStatus::Queued);
    assert_eq!(requeued.pane_id, None);
    assert_eq!(requeued.batch_id, None);
    assert_eq!(requeued.last_error.as_deref(), Some("pane missing"));
    assert!(
        q.history()
            .iter()
            .any(|message| message.message_id == abandoned.message_id
                && message.status == MessageStatus::Abandoned)
    );
}
