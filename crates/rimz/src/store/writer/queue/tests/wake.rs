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
