//! Synthetic round-trip: write a feed item, resolve it via the ledger, read
//! the event log back. Proves atomic write + length-framed log + snapshot
//! rebuild work end to end.

use rimz::{
    AbandonReason, EventEnvelope, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod,
    ResolverId, ResolverStep, ResolverStepState, Surface,
};
use serde_json::json;

fn chain_step(id: &ResolverId, order: i32, budget_ms: u64) -> ResolverStep {
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
    let h = crate::common::Harness::new();

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
    let h = crate::common::Harness::new();
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
    let h = crate::common::Harness::new();

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
    let h = crate::common::Harness::new();

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
    let h = crate::common::Harness::new();

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
    let h = crate::common::Harness::new();
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
fn abstain_advances_chain_and_records_audit_event() {
    let h = crate::common::Harness::new();
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
    let h = crate::common::Harness::new();
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
    let h = crate::common::Harness::new();
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
    let h = crate::common::Harness::new();
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
    let h = crate::common::Harness::new();
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

#[test]
fn expire_agent_session_abandons_only_that_sessions_pending_asks() {
    let h = crate::common::Harness::new();

    // Two pending claude asks from the session that is about to end, plus an
    // ask from a different live session that must survive.
    let mut ended_a = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    ended_a.payload = json!({ "session_id": "ended" });
    let mut ended_b = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Question,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    ended_b.payload = json!({ "session_id": "ended" });
    let mut other = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    other.payload = json!({ "session_id": "live" });

    for item in [&ended_a, &ended_b, &other] {
        h.ledger.push_feed_item(item, "rimz-test").expect("push");
    }

    let expired = h
        .ledger
        .expire_agent_session("claude", "ended", "rimz-test")
        .expect("expire");
    assert_eq!(expired, 2, "both asks from the ended session are expired");

    let by_id = |id: &rimz::RequestId| h.ledger.load_feed_item(id).expect("load").status;
    assert_eq!(by_id(&ended_a.request_id), FeedStatus::Abandoned);
    assert_eq!(by_id(&ended_b.request_id), FeedStatus::Abandoned);
    assert_eq!(
        by_id(&other.request_id),
        FeedStatus::Pending,
        "a different live session keeps its ask"
    );

    let events = h.ledger.read_events().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.method == "feed.expire")
            .count(),
        2,
    );

    // Idempotent: re-running expires nothing (no longer pending).
    let again = h
        .ledger
        .expire_agent_session("claude", "ended", "rimz-test")
        .expect("expire again");
    assert_eq!(again, 0);
}

#[test]
fn moved_on_expiry_clears_native_ui_asks_but_spares_bridge() {
    let h = crate::common::Harness::new();

    // A live session that just moved on left two native_ui asks pending and one
    // bridge ask in flight; the bridge ask resolves via its own socket and must
    // survive the move-on expiry.
    let mut native_a = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    native_a.payload = json!({ "session_id": "live" });
    let mut native_b = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Question,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    native_b.payload = json!({ "session_id": "live" });
    let mut bridge = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "claude needs attention",
        "claude",
        "agent-hook",
    );
    bridge.payload = json!({ "session_id": "live" });

    for item in [&native_a, &native_b, &bridge] {
        h.ledger.push_feed_item(item, "rimz-test").expect("push");
    }

    let expired = h
        .ledger
        .expire_agent_native_ui_asks("claude", "live", "rimz-test")
        .expect("expire");
    assert_eq!(expired, 2, "only the two native_ui asks are expired");

    let by_id = |id: &rimz::RequestId| h.ledger.load_feed_item(id).expect("load").status;
    assert_eq!(by_id(&native_a.request_id), FeedStatus::Abandoned);
    assert_eq!(by_id(&native_b.request_id), FeedStatus::Abandoned);
    assert_eq!(
        by_id(&bridge.request_id),
        FeedStatus::Pending,
        "the in-flight bridge ask keeps resolving through its socket",
    );

    // The audit trail names the move-on reason, not a session end.
    let events = h.ledger.read_events().expect("events");
    assert!(
        events.iter().any(|event| event.method == "feed.expire"
            && event.params.get("reason").and_then(|v| v.as_str()) == Some("agent_moved_on")),
        "expiry is audited as agent_moved_on",
    );
}

#[test]
fn runtime_projection_reads_under_the_workspace_lock() {
    // The sidebar's read projection must take the workspace lock for its read,
    // so a concurrent writer's half-written trailing event-log record is never
    // observed as a torn record and silently dropped — the race that flashed a
    // live agent as a bare `process` row that vanished next tick. Proven by
    // holding the lock and confirming the projection blocks until it frees: a
    // second flock acquire conflicts even within one process.
    use std::sync::mpsc;
    use std::time::Duration;

    let h = crate::common::Harness::new();

    // One committed agent, so a clean projection has an agent to lose.
    let obs = rimz::agents::AgentLifecycleObservation {
        agent_id: Some("agent-1".to_owned()),
        status: rimz::feed::AgentStatus::Idle,
        permission_posture: Some(rimz::feed::PermissionPosture::Default),
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        pane_id: None,
        parent_agent_id: None,
    };
    let envelope = EventEnvelope::agent_lifecycle(
        h.workspace_id.clone(),
        "rimz-test",
        "claude",
        "SessionStart",
        &obs,
    );
    h.ledger.append_event(&envelope).expect("append agent");

    // Hold the lock; the projection cannot acquire it until we release.
    let guard = rimz::ledger::lock::WorkspaceLock::acquire(h.ledger.workspace_lock_path())
        .expect("hold workspace lock");

    let ledger = h.ledger.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        started_tx.send(()).expect("signal start");
        let projection = ledger.runtime_projection(rimz::RuntimeScope::Runtime);
        let _ = result_tx.send(projection.map(|p| p.agents.len()));
    });

    // Once the reader is running, the projection must stay blocked behind the
    // held lock; a lock-free regression would deliver a (possibly torn) result.
    started_rx.recv().expect("reader started");
    assert!(
        result_rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "runtime_projection returned while the workspace lock was held — it is reading lock-free",
    );

    // Release; the projection completes and still sees the committed agent.
    drop(guard);
    let agents = result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("projection completes once the lock frees")
        .expect("projection succeeds");
    assert_eq!(
        agents, 1,
        "the committed agent survives the serialized read"
    );
    reader.join().expect("reader thread");
}
