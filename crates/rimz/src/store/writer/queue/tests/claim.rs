use super::*;

#[test]
fn batch_hold_check_is_read_only_and_rejects_any_lost_claim() {
    let q = Queue::new();
    let first = q.queue(1);
    q.queue(2);
    assert!(!q.message_claims_held(std::slice::from_ref(&first)).unwrap());
    let held = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
        .unwrap()
        .unwrap();
    let live = q.live();
    let events = q.methods();
    assert!(q.message_claims_held(&held).unwrap());
    assert_eq!(q.live(), live);
    assert_eq!(q.methods(), events);

    for change_attempt in [true, false] {
        let mut stale = held.clone();
        if change_attempt {
            stale[1].attempts += 1;
        } else {
            stale[1].last_attempt_at = Some(Timestamp::now() - CLAIM_TTL);
        }
        assert!(!q.message_claims_held(&stale).unwrap());
    }
    q.record_sent_batch(&held[1..], "session").unwrap();
    assert!(!q.message_claims_held(&held).unwrap());
    q.clear_messages_for(
        &first.kind,
        &first.agent_id,
        first.agent_name.as_deref(),
        "session",
    )
    .unwrap();
    assert!(!q.message_claims_held(&held).unwrap());
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

    q.record_message_delivery_failure(&claimed, "pane missing", "session")
        .unwrap();
    let pending = q.list_pending_messages().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert_eq!(pending[0].last_error.as_deref(), Some("pane missing"));
}

#[test]
fn steer_claim_skips_fifo_and_schedule_and_reclaims_after_failure() {
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
    q.record_message_delivery_failure(&claimed, "pane missing", "session")
        .unwrap();
    assert!(
        q.claim_message_for_steer(&second.message_id, Timestamp::now())
            .unwrap()
            .is_some(),
        "a failed claim is immediately available"
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
        q.record_message_delivery_failure(&claimed, "pane missing", "session")
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
fn sent_prompt_holds_command_claim_until_turn_start_ack() {
    let q = Queue::new();
    let first = q.queue(1);
    let claimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
        .unwrap()
        .expect("prompt claimed");
    let sent = q.record_sent_batch(&claimed, "session").unwrap();
    let second = q.queue_with(2, |message| message.body = MessageBody::Command);
    assert!(
        q.claim_delivery_batch(&second.message_id, AgentStatus::Idle, Timestamp::now())
            .unwrap()
            .is_none(),
        "no boundary write passes an unacknowledged prompt"
    );
    assert_eq!(q.by_id(&second.message_id).attempts, 0);
    let delivered = q
        .confirm_delivered_for_card(
            &first.kind,
            &first.agent_id,
            first.agent_name.as_deref(),
            DeliveryAck::TurnStarted { prompt: None },
            "session",
        )
        .unwrap();
    assert_eq!(delivered.len(), sent.len());
    assert_eq!(delivered[0].message_id, first.message_id);
    assert_eq!(delivered[0].status, MessageStatus::Delivered);
    assert!(
        q.claim_delivery_batch(&second.message_id, AgentStatus::Idle, Timestamp::now())
            .unwrap()
            .is_some()
    );
}

#[test]
fn sent_prompt_reconciliation_preserves_fifo_or_releases_timed_out_hold() {
    for unconfirmed_sends in [0, 1] {
        let q = Queue::new();
        let first = q.queue_with(1, |message| message.unconfirmed_sends = unconfirmed_sends);
        let claimed = q
            .claim_delivery_batch(&first.message_id, AgentStatus::Idle, Timestamp::now())
            .unwrap()
            .expect("prompt claimed");
        let sent = q.record_sent_batch(&claimed, "session").unwrap();
        let second = q.queue_with(2, |message| message.body = MessageBody::Command);
        assert!(
            q.claim_delivery_batch(&second.message_id, AgentStatus::Idle, Timestamp::now())
                .unwrap()
                .is_none()
        );
        let deadline = sent[0].last_sent_at.unwrap() + MessageBody::Prompt.delivery_window();
        let report = q.reconcile_stale_messages("session", deadline, 1).unwrap();
        if unconfirmed_sends == 1 {
            assert_eq!(report.timed_out, 1);
            assert!(
                q.claim_delivery_batch(&second.message_id, AgentStatus::Idle, deadline)
                    .unwrap()
                    .is_some()
            );
            continue;
        }
        assert_eq!(report.requeued, 1);
        assert_eq!(q.by_id(&first.message_id).status, MessageStatus::Queued);
        assert!(
            q.claim_delivery_batch(&second.message_id, AgentStatus::Idle, deadline)
                .unwrap()
                .is_none(),
            "reconciliation requeues the older prompt ahead of the command"
        );
        assert!(
            q.claim_delivery_batch(&first.message_id, AgentStatus::Idle, deadline)
                .unwrap()
                .is_some()
        );
    }
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

    let released = q
        .release_message_claims(&claimed, "waiting for compaction", "session")
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

    let result = q
        .record_message_delivery_failures(
            &claimed,
            DeliveryFailureDisposition::Retry,
            "pane missing",
            "session",
        )
        .unwrap();

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

#[test]
fn claim_stamp_survives_the_queue_file() {
    let q = Queue::new();
    let message = q.queue(1);
    let claimed = q
        .claim_message_for_delivery(&message.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");

    let on_disk = q.by_id(&message.message_id);

    assert!(claimed.last_attempt_at.is_some());
    assert_eq!(on_disk.attempts, claimed.attempts);
    assert_eq!(on_disk.last_attempt_at, claimed.last_attempt_at);
    q.record_message_delivery_failure(&claimed, "write failed", "session")
        .unwrap();
    let requeued = q.by_id(&message.message_id);
    assert_eq!(requeued.status, MessageStatus::Queued);
    assert_eq!(requeued.last_error.as_deref(), Some("write failed"));
    assert_eq!(requeued.attempts, 1);
    assert_eq!(requeued.last_attempt_at, None);
    let reclaimed = q
        .claim_message_for_delivery(&message.message_id, claimed.last_attempt_at.unwrap())
        .unwrap()
        .expect("immediate retry");
    assert_eq!(reclaimed.attempts, 2);
    assert_eq!(
        q.record_send_error(&claimed, "stale", "session").unwrap(),
        None
    );
    assert_eq!(q.by_id(&message.message_id), reclaimed);
}

/// Another sender's claim on `claimed`'s record: still `Claimed` under that stamp, with no terminal
/// entry and no event beyond the original queueing.
fn assert_claim_untouched(q: &Queue, claimed: &MessageRecord) {
    let live = q.by_id(&claimed.message_id);
    assert_eq!(live.status, MessageStatus::Claimed);
    assert_eq!(live.attempts, claimed.attempts);
    assert_eq!(live.last_attempt_at, claimed.last_attempt_at);
    assert!(q.history().is_empty());
    assert_eq!(q.count("message.errored"), 0);
    assert_eq!(q.count("message.queued"), q.live().len());
}

/// `tail` claimed by A at `now`, then re-claimed by B's batch after the TTL. Returns A's stale record
/// and B's record of the tail.
fn tail_reclaimed_after_ttl(q: &Queue) -> (MessageRecord, MessageRecord) {
    let first = q.queue(1);
    let tail = q.queue(2);
    let now = Timestamp::now();
    let stale = q
        .claim_message_for_steer(&tail.message_id, now)
        .unwrap()
        .expect("A claims the tail");
    let reclaimed = q
        .claim_delivery_batch(&first.message_id, AgentStatus::Idle, now + CLAIM_TTL)
        .unwrap()
        .expect("B claims the batch after the TTL")
        .remove(1);
    assert_eq!(reclaimed.message_id, tail.message_id);
    (stale, reclaimed)
}

#[test]
fn unheld_miss_leaves_another_senders_claim_and_its_sent_lands() {
    let q = Queue::new();
    let message = q.queue(1);
    let claimed = q
        .claim_message_for_delivery(&message.message_id, Timestamp::now())
        .unwrap()
        .expect("B claims");

    let result = q
        .record_unheld_delivery_miss(
            &message.message_id,
            DeliveryFailureDisposition::Terminal,
            "gate closed",
            "session",
        )
        .unwrap();

    assert!(!result.head_sent);
    assert_claim_untouched(&q, &claimed);
    let sent = q
        .record_sent_batch(std::slice::from_ref(&claimed), "session")
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(q.by_id(&message.message_id).status, MessageStatus::Sent);
    assert_eq!(q.count("message.sent"), 1);
    assert!(q.history().is_empty());
}

#[test]
fn stale_hold_gives_up_nothing_and_the_new_claims_sent_lands() {
    let q = Queue::new();
    let (stale, reclaimed) = tail_reclaimed_after_ttl(&q);

    let failure = q
        .record_message_delivery_failures(
            std::slice::from_ref(&stale),
            DeliveryFailureDisposition::Terminal,
            "write failed",
            "session",
        )
        .unwrap();
    let send_error = q
        .record_send_error(&stale, "pane vanished", "session")
        .unwrap();
    let released = q
        .release_message_claims(std::slice::from_ref(&stale), "parked", "session")
        .unwrap();

    assert!(!failure.head_sent);
    assert_eq!(send_error, None);
    assert!(released.is_empty());
    assert_claim_untouched(&q, &reclaimed);
    q.record_sent_batch(std::slice::from_ref(&reclaimed), "session")
        .unwrap();
    assert_eq!(q.by_id(&reclaimed.message_id).status, MessageStatus::Sent);
    assert_eq!(q.count("message.sent"), 1);
    assert!(q.history().is_empty());
}

#[test]
fn an_unclaimed_record_is_held_by_nobody() {
    let q = Queue::new();
    let fresh = q.queue(1);

    let send_error = q.record_send_error(&fresh, "waiting", "session").unwrap();

    assert_eq!(send_error, None);
    assert_eq!(q.live(), vec![fresh]);
    assert!(q.history().is_empty());
    assert_eq!(q.methods(), ["message.queued"]);
}

#[test]
fn fresh_boundary_claim_parks_behind_either_id_order() {
    for (winner_id, loser_id) in [(1, 2), (2, 1)] {
        let q = Queue::new();
        let now = Timestamp::from_second(1_000).unwrap();
        let held = q
            .queue_claimed_message(&q.record(winner_id), "session", now)
            .unwrap();
        let fresh = q.record(loser_id);
        let BoundaryClaim::Parked {
            message: parked,
            blocker,
        } = q.queue_boundary_message(&fresh, "session", now).unwrap()
        else {
            panic!("fresh boundary send must park behind the claim");
        };
        assert_eq!(blocker, held.message_id);
        assert!(parked.message_id.as_str() > held.message_id.as_str());
        assert_ne!(parked.message_id, fresh.message_id);
        assert_eq!(q.by_id(&parked.message_id), parked);
        assert_eq!(parked.status, MessageStatus::Queued);
        assert_eq!(parked.attempts, 0);
        assert_eq!(parked.last_attempt_at, None);
        assert!(
            !q.message_claims_held(std::slice::from_ref(&parked))
                .unwrap()
        );
        assert_eq!(q.methods(), ["message.queued", "message.queued"]);
        let events = q.events();
        assert_eq!(
            events.last().unwrap().params_value()["message_id"],
            parked.message_id.as_str(),
            "{events:?}"
        );
    }
}

#[test]
fn fresh_boundary_claim_parks_behind_a_sent_prompt() {
    let q = Queue::new();
    let sent = q.sent(2);
    let fresh = q.record(1);
    let BoundaryClaim::Parked {
        message: parked,
        blocker,
    } = q
        .queue_boundary_message(&fresh, "session", Timestamp::now())
        .unwrap()
    else {
        panic!("fresh boundary send must park behind the sent prompt");
    };
    assert_eq!(blocker, sent.message_id);
    assert!(parked.message_id.as_str() > sent.message_id.as_str());
    let mut fresh = fresh;
    fresh.message_id = parked.message_id;
    assert_eq!(q.by_id(&fresh.message_id), fresh);
    assert!(!q.message_claims_held(std::slice::from_ref(&fresh)).unwrap());
    assert_eq!(
        q.methods(),
        ["message.queued", "message.sent", "message.queued"]
    );
}

#[test]
fn fresh_boundary_claim_prefers_a_deliverable_queue_head() {
    let q = Queue::new();
    let head = q.queue(1);
    let now = Timestamp::from_second(1_000).unwrap();
    q.queue_claimed_message(&q.record(2), "session", now)
        .unwrap();
    let mut tail = q.record(4);
    tail.message_id = MessageId::parse("msg_v000000000000000").unwrap();
    tail.not_before = Some(now + CLAIM_TTL);
    q.queue_message(&tail, "session").unwrap();
    let mut other = q.record(5);
    other.message_id = MessageId::parse("msg_v000000000000002").unwrap();
    other.agent_id = "other-session".into();
    q.queue_message(&other, "session").unwrap();
    let fresh = q.record(3);
    let BoundaryClaim::Parked {
        message: parked,
        blocker,
    } = q.queue_boundary_message(&fresh, "session", now).unwrap()
    else {
        panic!("fresh boundary send must park behind the queue head");
    };
    assert_eq!(blocker, head.message_id);
    assert!(parked.message_id.as_str() > tail.message_id.as_str());
    assert!(parked.message_id.as_str() < other.message_id.as_str());
    let mut fresh = fresh;
    fresh.message_id = parked.message_id;
    assert_eq!(q.by_id(&fresh.message_id), fresh);
    assert!(!q.message_claims_held(std::slice::from_ref(&fresh)).unwrap());
    assert_eq!(
        q.methods(),
        [
            "message.queued",
            "message.queued",
            "message.queued",
            "message.queued",
            "message.queued"
        ]
    );
}

#[test]
fn fresh_boundary_claim_ignores_records_outside_its_blocking_lane() {
    for scenario in [
        "empty",
        "expired",
        "resume",
        "other-card",
        "command",
        "scheduled",
        "self",
    ] {
        let q = Queue::new();
        let now = Timestamp::from_second(1_000).unwrap();
        let mut prior = q.record(1);
        let fresh = q.record(2);
        match scenario {
            "empty" => {}
            "expired" => {
                prior.message_id = MessageId::parse("msg_v000000000000000").unwrap();
                q.queue_claimed_message(&prior, "session", now - CLAIM_TTL)
                    .unwrap();
            }
            "resume" => {
                prior.gate = DeliveryGate::Resume;
                q.queue_claimed_message(&prior, "session", now).unwrap();
            }
            "other-card" => {
                prior.agent_id = "other-session".into();
                q.queue_claimed_message(&prior, "session", now).unwrap();
            }
            "command" => {
                q.sent_with(1, |message| message.body = MessageBody::Command);
            }
            "scheduled" => {
                prior.not_before = Some(now + CLAIM_TTL);
                q.queue_message(&prior, "session").unwrap();
            }
            "self" => {
                q.queue_message(&fresh, "session").unwrap();
            }
            _ => unreachable!(),
        }
        let queued_events = q.count("message.queued");
        let BoundaryClaim::Claimed(claimed) =
            q.queue_boundary_message(&fresh, "session", now).unwrap()
        else {
            panic!("fresh boundary send must claim in {scenario}");
        };
        assert_ne!(claimed.message_id, fresh.message_id, "{scenario}");
        if scenario == "expired" {
            assert!(claimed.message_id.as_str() > prior.message_id.as_str());
        }
        let mut expected = fresh.clone();
        expected.message_id = claimed.message_id.clone();
        expected.status = MessageStatus::Claimed;
        expected.attempts = 1;
        expected.last_attempt_at = Some(now);
        expected.updated_at = now;
        assert_eq!(claimed, expected, "{scenario}");
        assert_eq!(q.by_id(&claimed.message_id), expected);
        assert!(
            !q.live()
                .iter()
                .any(|live| live.message_id == fresh.message_id)
        );
        assert!(
            q.message_claims_held(std::slice::from_ref(&expected))
                .unwrap()
        );
        assert_eq!(q.count("message.queued"), queued_events + 1);
        let events = q.events();
        assert_eq!(
            events.last().unwrap().params_value()["message_id"],
            claimed.message_id.as_str(),
            "{events:?}"
        );
    }
}

#[test]
fn enqueue_and_claim_excludes_other_deliverers_until_sent() {
    let q = Queue::new();
    let now = Timestamp::from_second(1_000).unwrap();
    let held = q
        .queue_claimed_message(&q.record(1), "session", now)
        .unwrap();

    assert_eq!(held.status, MessageStatus::Claimed);
    assert_eq!(held.attempts, 1);
    assert_eq!(held.last_attempt_at, Some(now));
    assert_eq!(q.by_id(&held.message_id), held);
    assert!(
        q.claim_delivery_batch(&held.message_id, AgentStatus::Idle, now)
            .unwrap()
            .is_none()
    );
    assert!(
        q.claim_message_for_steer(&held.message_id, now)
            .unwrap()
            .is_none()
    );
    assert!(
        message::queue_head(
            q.live().iter(),
            &held.kind,
            &held.agent_id,
            held.agent_name.as_deref(),
            now
        )
        .is_none()
    );
    assert!(q.list_pending_messages().unwrap().is_empty());
    let sent = q
        .record_sent_batch(std::slice::from_ref(&held), "session")
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].status, MessageStatus::Sent);
    assert_eq!(q.methods(), ["message.queued", "message.sent"]);
}

#[test]
fn fresh_claim_holder_can_error_or_retry_once() {
    for disposition in [
        DeliveryFailureDisposition::Terminal,
        DeliveryFailureDisposition::Retry,
    ] {
        let q = Queue::new();
        let now = Timestamp::from_second(1_000).unwrap();
        let held = q
            .queue_claimed_message(&q.record(1), "session", now)
            .unwrap();
        assert_eq!(held.status, MessageStatus::Claimed);
        if disposition == DeliveryFailureDisposition::Terminal {
            let errored = q
                .record_send_error(&held, "failed", "session")
                .unwrap()
                .unwrap();
            assert_eq!(errored.status, MessageStatus::Errored);
            assert_eq!(
                q.record_send_error(&held, "again", "session").unwrap(),
                None
            );
            assert_eq!(q.history(), vec![errored]);
            assert_eq!(q.count("message.errored"), 1);
            continue;
        }
        q.record_message_delivery_failure(&held, "failed", "session")
            .unwrap();
        let queued = q.by_id(&held.message_id);
        assert_eq!(queued.status, MessageStatus::Queued);
        assert_eq!(queued.attempts, 1);
        assert_eq!(queued.last_attempt_at, None);
        let methods = q.methods();
        q.record_message_delivery_failure(&held, "stale", "session")
            .unwrap();
        assert_eq!(q.by_id(&held.message_id), queued);
        assert_eq!(q.methods(), methods);
    }
}

#[test]
fn failure_after_sent_leaves_the_record_sent_and_reports_it() {
    let q = Queue::new();
    let (stale, reclaimed) = tail_reclaimed_after_ttl(&q);
    q.record_sent_batch(std::slice::from_ref(&reclaimed), "session")
        .unwrap();

    let unheld = q
        .record_unheld_delivery_miss(
            &reclaimed.message_id,
            DeliveryFailureDisposition::Terminal,
            "gate closed",
            "session",
        )
        .unwrap();
    let stale_held = q
        .record_message_delivery_failures(
            std::slice::from_ref(&stale),
            DeliveryFailureDisposition::Terminal,
            "write failed",
            "session",
        )
        .unwrap();

    assert!(unheld.head_sent);
    assert!(stale_held.head_sent);
    assert_eq!(q.by_id(&reclaimed.message_id).status, MessageStatus::Sent);
    assert!(q.history().is_empty());
    assert_eq!(q.count("message.errored"), 0);
}

#[test]
fn sent_after_cancel_stays_canceled_and_creates_nothing() {
    let q = Queue::new();
    let message = q.queue(1);
    let claimed = q
        .claim_message_for_delivery(&message.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    assert!(
        q.cancel_message(&message.message_id, "session", "user canceled")
            .unwrap()
    );

    let sent = q
        .record_sent_batch(std::slice::from_ref(&claimed), "session")
        .unwrap();

    assert!(sent.is_empty());
    assert!(q.live().is_empty());
    let history = q.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message_id, message.message_id);
    assert_eq!(history[0].status, MessageStatus::Canceled);
    assert_eq!(q.count("message.sent"), 0);
}

#[test]
fn sent_for_a_record_never_queued_creates_nothing() {
    let q = Queue::new();

    let sent = q.record_sent_batch(&[q.record(1)], "session").unwrap();

    assert!(sent.is_empty());
    assert!(q.live().is_empty());
    assert!(q.events().is_empty());
}

#[test]
fn unheld_miss_settles_a_queued_record_by_its_disposition() {
    let q = Queue::new();
    let retried = q.queue(1);
    let errored = q.queue(2);

    for (record, disposition) in [
        (&retried, DeliveryFailureDisposition::Retry),
        (&errored, DeliveryFailureDisposition::Terminal),
    ] {
        q.record_unheld_delivery_miss(&record.message_id, disposition, "gate closed", "session")
            .unwrap();
    }

    let live = q.live();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].message_id, retried.message_id);
    assert_eq!(live[0].status, MessageStatus::Queued);
    assert_eq!(live[0].last_error.as_deref(), Some("gate closed"));
    let history = q.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message_id, errored.message_id);
    assert_eq!(history[0].status, MessageStatus::Errored);
    assert_eq!(q.count("message.errored"), 1);
}
