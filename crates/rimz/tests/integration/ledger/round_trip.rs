//! Synthetic round-trip: write a feed item, resolve it via the ledger, read
//! the event log back. Proves atomic write + length-framed log + snapshot
//! rebuild work end to end.

use rimz::{AbandonReason, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, Surface};
use serde_json::json;

fn script_ask(h: &crate::common::Harness, title: &str) -> FeedItem {
    FeedItem::new(
        h.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        title,
        "rimz",
        "cli",
    )
}

fn native_agent_ask(
    h: &crate::common::Harness,
    kind: FeedKind,
    title: &str,
    session_id: &str,
) -> FeedItem {
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        kind,
        title,
        "claude",
        "agent-hook",
    );
    item.payload = json!({ "session_id": session_id });
    item
}

fn status(h: &crate::common::Harness, id: &rimz::RequestId) -> FeedStatus {
    h.ledger.load_feed_item(id).expect("load").status
}

#[test]
fn push_then_resolve_round_trip() {
    let h = crate::common::Harness::new();

    let item = script_ask(&h, "allow rm -rf node_modules?");
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let listed = h.ledger.list_feed_items().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "allow rm -rf node_modules?");
    assert_eq!(listed[0].status.to_string(), "pending");

    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::Cli);
    let outcome = h
        .ledger
        .resolve_feed_item(&request_id, resolution, "rimz-test")
        .expect("resolve");
    assert!(outcome.effective);
    assert!(!outcome.late);

    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(after.status.to_string(), "resolved");
    assert!(after.resolution.is_some());

    let events = h.ledger.read_events().expect("read events");
    assert!(events.iter().any(|e| e.method == "feed.push"));
    let resolve = events
        .iter()
        .find(|e| e.method == "feed.resolve")
        .expect("resolve event");
    assert_eq!(resolve.session_name, "rimz-test");
}

#[test]
fn native_ui_requests_can_record_pane_answer_or_be_dismissed() {
    let h = crate::common::Harness::new();

    let answer = native_agent_ask(&h, FeedKind::Permission, "answer me", "sess-1");
    let answer_id = answer.request_id.clone();
    h.ledger
        .push_feed_item(&answer, "rimz-test")
        .expect("push answer");

    let mut resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::PaneSend);
    resolution.by = Some("auto-policy".to_owned());
    let outcome = h
        .ledger
        .resolve_feed_item(&answer_id, resolution, "rimz-test")
        .expect("resolve native_ui");
    assert!(outcome.effective);
    let after = h.ledger.load_feed_item(&answer_id).expect("reload");
    assert_eq!(after.status, FeedStatus::Resolved);
    let resolution = after.resolution.expect("resolution");
    assert_eq!(resolution.by.as_deref(), Some("auto-policy"));
    assert_eq!(resolution.method, ResolutionMethod::PaneSend);

    let dismiss = native_agent_ask(&h, FeedKind::Permission, "dismiss me", "sess-2");
    let dismiss_id = dismiss.request_id.clone();
    h.ledger
        .push_feed_item(&dismiss, "rimz-test")
        .expect("push dismiss");
    h.ledger
        .dismiss_feed_item(&dismiss_id, Some("not now".into()), "rimz-test")
        .expect("dismiss");
    let after = h.ledger.load_feed_item(&dismiss_id).expect("reload");
    assert_eq!(after.status, FeedStatus::Resolved);
    assert_eq!(
        after.resolution.as_ref().unwrap().method,
        ResolutionMethod::Dismiss
    );
}

#[test]
fn timeout_marks_script_item_and_late_answer_is_audit_only() {
    let h = crate::common::Harness::new();

    let item = script_ask(&h, "deploy?");
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let timeout = h
        .ledger
        .mark_feed_item_timed_out(&request_id, "rimz-test", AbandonReason::ScriptWaitTimeout)
        .expect("timeout");
    assert!(timeout.transitioned);
    assert_eq!(timeout.status, FeedStatus::TimedOut);

    let timed_out = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(timed_out.status, FeedStatus::TimedOut);
    assert!(timed_out.resolution.is_none());

    let late = h
        .ledger
        .resolve_feed_item(
            &request_id,
            Resolution::new(json!({ "choice": "yes" }), ResolutionMethod::Cli),
            "rimz-test",
        )
        .expect("late resolve");
    assert!(!late.effective);
    assert!(late.late);

    let after_late = h.ledger.load_feed_item(&request_id).expect("reload late");
    assert_eq!(after_late.status, FeedStatus::TimedOut);
    assert!(after_late.resolution.is_none());

    let events = h.ledger.read_events().expect("read events");
    assert!(events.iter().any(|event| event.method == "feed.timeout"));
    let resolve = events
        .iter()
        .find(|event| event.method == "feed.resolve")
        .expect("late resolve event");
    let params = resolve.params_value();
    assert_eq!(params["effective"], false);
    assert_eq!(params["late"], true);
    assert_eq!(params["reason"], "script_wait_timeout");
}

#[test]
fn wakeup_failure_does_not_fail_committed_push() {
    let h = crate::common::Harness::new();
    std::fs::remove_dir(&h.runtime_paths.heartbeat_dir).expect("remove heartbeat dir");
    std::fs::write(&h.runtime_paths.heartbeat_dir, b"not a dir").expect("replace with file");

    let item = native_agent_ask(
        &h,
        FeedKind::Permission,
        "wakeups are best effort",
        "sess-1",
    );
    let request_id = item.request_id.clone();

    h.ledger
        .push_feed_item(&item, "rimz-test")
        .expect("push should commit even when wakeup walk fails");

    let after = h
        .ledger
        .load_feed_item(&request_id)
        .expect("committed item");
    assert_eq!(after.title, "wakeups are best effort");
}

#[test]
fn concurrent_resolve_uses_first_writer_wins_cas() {
    let h = crate::common::Harness::new();
    let item = script_ask(&h, "ship?");
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let mut joins = Vec::new();
    for choice in ["yes", "no"] {
        let ledger = h.ledger.clone();
        let request_id = request_id.clone();
        joins.push(std::thread::spawn(move || {
            ledger.resolve_feed_item(
                &request_id,
                Resolution::new(json!({ "choice": choice }), ResolutionMethod::Cli),
                "rimz-test",
            )
        }));
    }

    let results: Vec<_> = joins
        .into_iter()
        .map(|join| join.join().expect("join"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

    let after = h.ledger.load_feed_item(&request_id).expect("load");
    assert_eq!(after.status, FeedStatus::Resolved);
    let events = h.ledger.read_events().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.method == "feed.resolve")
            .count(),
        1
    );
}

#[test]
fn expiry_scopes_agent_session_end_and_moved_on_native_ui() {
    let h = crate::common::Harness::new();

    let ended_a = native_agent_ask(&h, FeedKind::Permission, "ended permission", "ended");
    let ended_b = native_agent_ask(&h, FeedKind::Question, "ended question", "ended");
    let other = native_agent_ask(&h, FeedKind::Permission, "other session", "other");
    for item in [&ended_a, &ended_b, &other] {
        h.ledger.push_feed_item(item, "rimz-test").expect("push");
    }

    let expired = h
        .ledger
        .expire_agent_session("claude", "ended", "rimz-test")
        .expect("expire");
    assert_eq!(expired, 2);
    assert_eq!(status(&h, &ended_a.request_id), FeedStatus::Abandoned);
    assert_eq!(status(&h, &ended_b.request_id), FeedStatus::Abandoned);
    assert_eq!(status(&h, &other.request_id), FeedStatus::Pending);
    assert_eq!(
        h.ledger
            .expire_agent_session("claude", "ended", "rimz-test")
            .expect("expire again"),
        0,
        "session-end expiry is idempotent"
    );

    let native_a = native_agent_ask(&h, FeedKind::Permission, "live permission", "live");
    let native_b = native_agent_ask(&h, FeedKind::Question, "live question", "live");
    let script = script_ask(&h, "live script");
    for item in [&native_a, &native_b, &script] {
        h.ledger.push_feed_item(item, "rimz-test").expect("push");
    }

    let expired = h
        .ledger
        .expire_agent_native_ui_asks("claude", "live", "rimz-test")
        .expect("expire");
    assert_eq!(expired, 2);
    assert_eq!(status(&h, &native_a.request_id), FeedStatus::Abandoned);
    assert_eq!(status(&h, &native_b.request_id), FeedStatus::Abandoned);
    assert_eq!(status(&h, &script.request_id), FeedStatus::Pending);

    let events = h.ledger.read_events().expect("events");
    assert!(
        ["agent_session_ended", "agent_moved_on"]
            .iter()
            .all(|reason| {
                events.iter().any(|event| {
                    event.method == "feed.expire"
                        && event.params_value().get("reason").and_then(|v| v.as_str())
                            == Some(*reason)
                })
            }),
        "both expiry reasons are audited",
    );
}

#[test]
fn runtime_projection_serves_lock_free_while_a_writer_holds_the_lock() {
    // Reads resume from the persisted rollup fold base, so they never take
    // the workspace lock: a projection completes — and still sees every
    // committed agent — while a writer holds the lock. The old hazard this
    // lock-held read guarded against (a torn in-flight tail silently
    // dropping an agent's only lifecycle event) is now structural: the fold
    // base already holds prior events, and an unfolded tail frame is folded
    // by the wakeup that completes it.
    use std::sync::mpsc;
    use std::time::Duration;

    let h = crate::common::Harness::new();

    h.ledger
        .append_event(&crate::common::lifecycle_event(
            &h,
            "rimz-test",
            "SessionStart",
            "agent-1",
        ))
        .expect("append agent");

    let _guard = rimz::ledger::lock::WorkspaceLock::acquire(h.ledger.workspace_lock_path())
        .expect("hold workspace lock");

    let ledger = h.ledger.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let projection = ledger.runtime_projection(rimz::RuntimeScope::Runtime);
        let _ = result_tx.send(projection.map(|p| p.agents.len()));
    });

    let agents = result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("projection completes while the workspace lock is held")
        .expect("projection succeeds");
    assert_eq!(agents, 1, "the committed agent survives the lock-free read");
    reader.join().expect("reader thread");
}

#[test]
fn late_resolve_after_abandon_is_rejected_not_audited() {
    // TimedOut keeps a script-ask audit window for late answers, while
    // Abandoned means the asker is gone.
    let h = crate::common::Harness::new();
    let item = native_agent_ask(&h, FeedKind::Permission, "owner left", "gone");
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    h.ledger
        .expire_agent_session("claude", "gone", "rimz-test")
        .expect("abandon via session end");
    assert_eq!(
        h.ledger.load_feed_item(&request_id).expect("load").status,
        FeedStatus::Abandoned
    );

    let late = h.ledger.resolve_feed_item(
        &request_id,
        Resolution::new(json!({ "choice": "yes" }), ResolutionMethod::Cli),
        "rimz-test",
    );
    assert!(
        matches!(
            late,
            Err(rimz::ledger::LedgerErr::FeedStore(
                rimz::ledger::FeedStoreErr::NotPending { .. }
            ))
        ),
        "an abandoned item rejects a late answer: {late:?}"
    );
    let events = h.ledger.read_events().expect("events");
    assert!(
        events.iter().all(|event| event.method != "feed.resolve"),
        "the rejection leaves no audit-only resolve record"
    );
}
