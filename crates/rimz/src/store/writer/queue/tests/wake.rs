use super::*;

fn stamp(q: &Queue) -> Option<Timestamp> {
    let path = q.runtime_paths().message_wake_path();
    assert!(path.is_file(), "queue commit must publish its wake stamp");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn queue_commit_arms_and_cancel_removes_wake() {
    let q = Queue::new();
    let not_before = Timestamp::now() + Duration::from_secs(120);
    let queued = q.queue_with(1, |record| record.not_before = Some(not_before));

    assert_eq!(stamp(&q), Some(not_before));
    assert!(
        q.cancel_message(&queued.message_id, "session", "canceled")
            .unwrap()
    );
    assert!(!q.runtime_paths().message_wake_path().exists());
}

#[test]
fn claim_commit_arms_recovery_before_returning() {
    let q = Queue::new();
    let queued = q.queue(1);
    let now = Timestamp::now();
    let claimed = q
        .claim_message_for_delivery(&queued.message_id, now)
        .unwrap()
        .expect("claimed");

    assert_eq!(
        stamp(&q),
        Some(claimed.last_attempt_at.unwrap() + CLAIM_TTL)
    );
}

#[test]
fn unwritable_stamp_never_fails_queue_or_cancel() {
    let q = Queue::new();
    let path = q.runtime_paths().message_wake_path();
    std::fs::create_dir_all(&path).unwrap();
    let queued = q.queue(1);

    assert_eq!(q.live(), std::slice::from_ref(&queued));
    assert!(
        q.cancel_message(&queued.message_id, "session", "canceled")
            .unwrap()
    );
    assert!(q.live().is_empty());
    assert!(path.is_dir());
}

#[test]
fn clock_rearm_moves_elapsed_floor_to_retry_without_queue_change() {
    let q = Queue::new();
    let now = Timestamp::now();
    let not_before = now + Duration::from_secs(120);
    let retry_after = not_before + Duration::from_secs(60);
    q.queue_with(1, |record| {
        record.not_before = Some(not_before);
        record.retry_after = Some(retry_after);
    });
    let path = q.runtime_paths().message_wake_path();
    crate::disk::atomic::write_temp_then_rename_cache(&path, &Some(not_before)).unwrap();
    let before = q.live();

    q.rearm_message_wake(not_before);

    assert_eq!(stamp(&q), Some(retry_after));
    assert_eq!(q.live(), before);
}

#[test]
fn deferred_head_ignores_ready_followers_but_keeps_independent_wakes() {
    let now = Timestamp::now();
    let retry_after = now + Duration::from_secs(120);
    let earlier = now + Duration::from_secs(20);
    let updated_at = now - Duration::from_secs(10);
    let cases = [
        ("ready", retry_after),
        ("expiry", earlier),
        ("registration", earlier),
        ("past registration", retry_after),
        ("scheduled", earlier),
        ("condition pending", updated_at),
        ("condition retry", earlier),
        ("claimed", retry_after.min(now + CLAIM_TTL)),
        (
            "sent",
            retry_after.min(now + MessageBody::Prompt.delivery_window()),
        ),
        ("resume", retry_after),
        ("other card", updated_at),
        ("other kind", updated_at),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(case, _)| {
            let q = Queue::new();
            q.queue_with(1, |record| record.retry_after = Some(retry_after));
            q.queue_with(2, |record| {
                record.updated_at = updated_at;
                match *case {
                    "expiry" => {
                        record.body = MessageBody::Command;
                        record.automated = true;
                        record.enqueued_at = earlier - crate::store::message::command_validity();
                    }
                    "registration" => record.register_by = Some(earlier),
                    "past registration" => record.register_by = Some(updated_at),
                    "scheduled" => record.not_before = Some(earlier),
                    "condition pending" | "condition retry" => {
                        record.after.push(AfterCondition {
                            kind: record.kind.clone(),
                            agent_id: record.agent_id.clone(),
                            agent_name: None,
                            address: "@watch".to_owned(),
                            met_at: None,
                        });
                        if *case == "condition retry" {
                            record.retry_after = Some(earlier);
                        }
                    }
                    "claimed" => {
                        record.status = MessageStatus::Claimed;
                        record.last_attempt_at = Some(now);
                    }
                    "sent" => {
                        record.status = MessageStatus::Sent;
                        record.last_sent_at = Some(now);
                    }
                    "resume" => record.gate = DeliveryGate::Resume,
                    "other card" => record.agent_id = "other".into(),
                    "other kind" => record.kind = AgentKind::new_unchecked("codex"),
                    _ => {}
                }
            });
            q.queue_with(3, |record| record.updated_at = updated_at);
            (*case, stamp(&q))
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(case, deadline)| (*case, Some(*deadline)))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn sent_oldest_and_deferred_head_ignore_ready_followers() {
    let q = Queue::new();
    let sent = q.sent(1);
    let reconcile_at = sent.sent_reconcile_deadline().unwrap();
    q.queue_with(2, |record| {
        record.retry_after = Some(reconcile_at + Duration::from_secs(60));
    });
    for id in [3, 4] {
        q.queue_with(id, |record| {
            record.updated_at = sent.updated_at - Duration::from_secs(10);
        });
    }

    assert_eq!(stamp(&q), Some(reconcile_at));
}

#[test]
fn sending_head_rearms_next_records_deadline_in_same_commit() {
    let q = Queue::new();
    let retry_after = Timestamp::now() + Duration::from_secs(120);
    let head = q.queue_with(1, |record| record.retry_after = Some(retry_after));
    let follower = q.queue_with(2, |record| {
        record.updated_at = Timestamp::now() - Duration::from_secs(10);
    });
    let before = stamp(&q);

    q.record_sent_message(&head, "session").unwrap().unwrap();

    assert_eq!(
        (before, stamp(&q)),
        (Some(retry_after), Some(follower.updated_at))
    );
}

#[test]
fn archiving_head_rearms_next_records_deadline_in_same_commit() {
    let q = Queue::new();
    let retry_after = Timestamp::now() + Duration::from_secs(120);
    let head = q.queue_with(1, |record| record.retry_after = Some(retry_after));
    let follower = q.queue_with(2, |record| {
        record.updated_at = Timestamp::now() - Duration::from_secs(10);
    });
    let before = stamp(&q);

    q.settle(&head.message_id, MessageStatus::Archived, Some("ended"))
        .unwrap();

    assert_eq!(
        (before, stamp(&q)),
        (Some(retry_after), Some(follower.updated_at))
    );
}
