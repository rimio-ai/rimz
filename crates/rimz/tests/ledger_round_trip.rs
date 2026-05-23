//! Synthetic round-trip: write a feed item, resolve it via the ledger, read
//! the event log back. Proves atomic write + length-framed log + snapshot
//! rebuild work end to end.

mod common;

use rimz::{
    AbandonReason, EventEnvelope, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod,
    ResolverId, ResolverStep, ResolverStepState, SidebarActivity, Surface,
};
use serde_json::json;

fn chain_step(id: &ResolverId, order: i32, budget_ms: i64) -> ResolverStep {
    ResolverStep {
        resolver_id: id.clone(),
        display_name: None,
        order,
        budget_ms,
        state: ResolverStepState::Queued,
        reason: None,
    }
}

#[test]
fn push_then_resolve_round_trip() {
    let h = common::Harness::new();

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "allow rm -rf node_modules?",
        "claude",
        "agent-hook",
    );
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let listed = h.ledger.list_feed_items().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "allow rm -rf node_modules?");
    assert_eq!(listed[0].status.to_string(), "pending");

    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::Cli);
    let outcome = h
        .ledger
        .resolve_feed_item(&request_id, resolution, true, "rimz-test")
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
fn resolving_active_resolver_marks_chain_answered_and_clears_active() {
    let h = common::Harness::new();
    let opus: ResolverId = "opus-policy".parse().unwrap();
    let slack: ResolverId = "slack-on-call".parse().unwrap();
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "chain resolve?",
        "claude",
        "agent-hook",
    );
    item.activate_resolver_chain(vec![
        chain_step(&opus, 10, 30_000),
        chain_step(&slack, 20, 300_000),
    ]);
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let mut resolution =
        Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    resolution.resolver_id = Some(opus.clone());
    h.ledger
        .resolve_feed_item(&request_id, resolution, false, "rimz-test")
        .expect("resolve");

    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(after.status, FeedStatus::Resolved);
    assert_eq!(after.chain[0].state, ResolverStepState::Answered);
    assert_eq!(after.chain[1].state, ResolverStepState::Queued);
    assert!(after.chain_active_resolver.is_none());
    assert!(after.chain_active_until.is_none());
}

#[test]
fn dismiss_only_applies_to_native_ui_surface() {
    let h = common::Harness::new();

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "dismiss me",
        "claude",
        "agent-hook",
    );
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    h.ledger
        .dismiss_feed_item(&request_id, Some("not now".into()), "rimz-test")
        .expect("dismiss");
    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(after.status.to_string(), "resolved");
    assert_eq!(
        after.resolution.as_ref().unwrap().method,
        ResolutionMethod::Dismiss
    );
    let events = h.ledger.read_events().expect("read events");
    assert!(
        events
            .iter()
            .any(|event| { event.method == "feed.dismiss" && event.session_name == "rimz-test" })
    );
}

#[test]
fn resolve_rejects_native_ui_surface() {
    let h = common::Harness::new();

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "native only",
        "claude",
        "agent-hook",
    );
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let result = h.ledger.resolve_feed_item(
        &request_id,
        Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::Cli),
        false,
        "rimz-test",
    );
    assert!(
        result.is_err(),
        "native_ui must reject `resolve` (got {result:?})"
    );
}

#[test]
fn timeout_marks_script_item_and_late_answer_is_audit_only() {
    let h = common::Harness::new();

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        "deploy?",
        "rimz",
        "cli",
    );
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let timeout = h
        .ledger
        .mark_feed_item_timed_out(&request_id, "rimz-test", AbandonReason::BridgeCapElapsed)
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
            true,
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
    assert_eq!(resolve.params["effective"], false);
    assert_eq!(resolve.params["late"], true);
}

#[test]
fn wakeup_failure_does_not_fail_committed_push() {
    let h = common::Harness::new();
    std::fs::remove_dir(&h.runtime_paths.heartbeat_dir).expect("remove heartbeat dir");
    std::fs::write(&h.runtime_paths.heartbeat_dir, b"not a dir").expect("replace with file");

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "wakeups are best effort",
        "claude",
        "agent-hook",
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
fn standalone_events_rebuild_recent_activity_snapshot() {
    let h = common::Harness::new();
    let event = EventEnvelope::new(
        h.workspace_id.clone(),
        "rimz-test",
        "rimz",
        "cli",
        "event.emit",
        json!({ "kind": "build.started", "title": "Building web" }),
    );

    h.ledger.append_event(&event).expect("append event");

    let snapshot = h.ledger.snapshot().expect("snapshot");
    assert!(snapshot.recent_activity.iter().any(|activity| matches!(
        activity,
        SidebarActivity::Event { event: seen }
            if seen.event_id == event.event_id && seen.session_name == "rimz-test"
    )));
}

#[test]
fn abstain_advances_chain_and_records_audit_event() {
    let h = common::Harness::new();
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "abstain?",
        "claude",
        "agent-hook",
    );
    let opus: ResolverId = "opus-policy".parse().unwrap();
    let slack: ResolverId = "slack-on-call".parse().unwrap();
    item.chain = vec![
        ResolverStep {
            resolver_id: opus.clone(),
            display_name: None,
            order: 10,
            budget_ms: 30_000,
            state: ResolverStepState::Active,
            reason: None,
        },
        ResolverStep {
            resolver_id: slack.clone(),
            display_name: None,
            order: 20,
            budget_ms: 300_000,
            state: ResolverStepState::Queued,
            reason: None,
        },
    ];
    item.chain_active_resolver = Some(opus.clone());
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let outcome = h
        .ledger
        .abstain_feed_item(
            &request_id,
            &opus,
            Some("out of policy band".to_owned()),
            "rimz-test",
        )
        .expect("abstain");
    assert_eq!(outcome.next_resolver.as_ref(), Some(&slack));

    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(after.status, FeedStatus::Pending);
    assert_eq!(after.chain[0].state, ResolverStepState::Abstained);
    assert_eq!(after.chain[1].state, ResolverStepState::Active);
    assert_eq!(after.chain_active_resolver.as_ref(), Some(&slack));
    assert!(after.chain_active_until.is_some());

    let events = h.ledger.read_events().expect("events");
    let abstain = events
        .iter()
        .find(|e| e.method == "feed.abstain")
        .expect("abstain event");
    assert_eq!(abstain.params["resolver_id"].as_str(), Some("opus-policy"));
    assert_eq!(
        abstain.params["next_resolver"].as_str(),
        Some("slack-on-call")
    );
}

#[test]
fn abstain_rejects_non_active_resolver() {
    let h = common::Harness::new();
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "abstain?",
        "claude",
        "agent-hook",
    );
    let opus: ResolverId = "opus-policy".parse().unwrap();
    let slack: ResolverId = "slack-on-call".parse().unwrap();
    item.chain = vec![
        ResolverStep {
            resolver_id: opus.clone(),
            display_name: None,
            order: 10,
            budget_ms: 30_000,
            state: ResolverStepState::Active,
            reason: None,
        },
        ResolverStep {
            resolver_id: slack.clone(),
            display_name: None,
            order: 20,
            budget_ms: 300_000,
            state: ResolverStepState::Queued,
            reason: None,
        },
    ];
    item.chain_active_resolver = Some(opus);
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let err = h
        .ledger
        .abstain_feed_item(&request_id, &slack, None, "rimz-test")
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not active"),
        "expected ResolverNotActive, got: {msg}"
    );
}

#[test]
fn abstain_with_no_next_resolver_exhausts_chain() {
    let h = common::Harness::new();
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "abstain?",
        "claude",
        "agent-hook",
    );
    let opus: ResolverId = "opus-policy".parse().unwrap();
    item.chain = vec![ResolverStep {
        resolver_id: opus.clone(),
        display_name: None,
        order: 10,
        budget_ms: 30_000,
        state: ResolverStepState::Active,
        reason: None,
    }];
    item.chain_active_resolver = Some(opus.clone());
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let outcome = h
        .ledger
        .abstain_feed_item(&request_id, &opus, None, "rimz-test")
        .expect("abstain");
    assert!(outcome.next_resolver.is_none());
    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert!(after.chain_active_resolver.is_none());
    assert!(after.chain_active_until.is_none());
    assert_eq!(after.chain[0].state, ResolverStepState::Abstained);
    // Feed item remains pending so the bridge can fall through to its cap.
    assert_eq!(after.status, FeedStatus::Pending);
}

#[test]
fn timeout_marks_active_resolver_budget_elapsed() {
    let h = common::Harness::new();
    let opus: ResolverId = "opus-policy".parse().unwrap();
    let slack: ResolverId = "slack-on-call".parse().unwrap();
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "timeout?",
        "claude",
        "agent-hook",
    );
    item.activate_resolver_chain(vec![
        chain_step(&opus, 10, 30_000),
        chain_step(&slack, 20, 300_000),
    ]);
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    h.ledger
        .mark_feed_item_timed_out(&request_id, "rimz-test", AbandonReason::BridgeCapElapsed)
        .expect("timeout");

    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(after.status, FeedStatus::TimedOut);
    assert_eq!(after.chain[0].state, ResolverStepState::BudgetElapsed);
    assert_eq!(after.chain[0].reason.as_deref(), Some("bridge_cap_elapsed"));
    assert_eq!(after.chain[1].state, ResolverStepState::Queued);
    assert!(after.chain_active_resolver.is_none());
    assert!(after.chain_active_until.is_none());
}

#[test]
fn concurrent_resolve_uses_first_writer_wins_cas() {
    let h = common::Harness::new();
    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        "ship?",
        "rimz",
        "cli",
    );
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
                true,
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
