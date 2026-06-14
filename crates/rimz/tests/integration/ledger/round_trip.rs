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

fn bridge_permission(h: &crate::common::Harness, title: &str) -> FeedItem {
    FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        title,
        "claude",
        "agent-hook",
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

    let item = bridge_permission(&h, "allow rm -rf node_modules?");
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
    let mut item = bridge_permission(&h, "chain resolve?");
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
fn native_ui_requests_are_dismiss_only() {
    let h = crate::common::Harness::new();

    let item = native_agent_ask(&h, FeedKind::Permission, "dismiss me", "sess-1");
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
fn timeout_marks_script_item_and_late_answer_is_audit_only() {
    let h = crate::common::Harness::new();

    let opus: ResolverId = "opus-policy".parse().unwrap();
    let slack: ResolverId = "slack-on-call".parse().unwrap();
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        "deploy?",
        "rimz",
        "cli",
    );
    item.activate_resolver_chain(vec![
        chain_step(&opus, 10, 30_000),
        chain_step(&slack, 20, 300_000),
    ]);
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
    assert_eq!(timed_out.chain[0].state, ResolverStepState::BudgetElapsed);
    assert_eq!(
        timed_out.chain[0].reason.as_deref(),
        Some("bridge_cap_elapsed")
    );
    assert_eq!(timed_out.chain[1].state, ResolverStepState::Queued);
    assert!(timed_out.chain_active_resolver.is_none());
    assert!(timed_out.chain_active_until.is_none());

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
fn abstain_advances_chain_and_records_audit_event() {
    let h = crate::common::Harness::new();
    let mut item = bridge_permission(&h, "abstain?");
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
fn abstain_with_no_next_resolver_exhausts_chain() {
    let h = crate::common::Harness::new();
    let mut item = bridge_permission(&h, "abstain?");
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
    let mut bridge = bridge_permission(&h, "live bridge");
    bridge.payload = json!({ "session_id": "live" });
    for item in [&native_a, &native_b, &bridge] {
        h.ledger.push_feed_item(item, "rimz-test").expect("push");
    }

    let expired = h
        .ledger
        .expire_agent_native_ui_asks("claude", "live", "rimz-test")
        .expect("expire");
    assert_eq!(expired, 2);
    assert_eq!(status(&h, &native_a.request_id), FeedStatus::Abandoned);
    assert_eq!(status(&h, &native_b.request_id), FeedStatus::Abandoned);
    assert_eq!(status(&h, &bridge.request_id), FeedStatus::Pending);

    let events = h.ledger.read_events().expect("events");
    assert!(
        ["agent_session_ended", "agent_moved_on"]
            .iter()
            .all(|reason| {
                events.iter().any(|event| {
                    event.method == "feed.expire"
                        && event.params.get("reason").and_then(|v| v.as_str()) == Some(*reason)
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

    // One committed agent, so a clean projection has an agent to lose.
    let obs = rimz::agents::AgentLifecycleObservation {
        agent_id: Some("agent-1".into()),
        agent_name: None,
        agent_alias: None,
        kind_ordinal: None,
        signal: rimz::agents::lifecycle::LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        transcript_path: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
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

    // Hold the workspace lock as a writer would mid-mutation.
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
    // The two answerless terminal states differ on purpose: `TimedOut` is
    // the bridge-cap audit window — a late answer is recorded
    // `effective: false` (timeout_marks_script_item_and_late_answer_is_audit_only)
    // — while `Abandoned` means the asker is gone, so a late answer has no
    // one to serve and is rejected outright, before any event append.
    let h = crate::common::Harness::new();
    let mut item = bridge_permission(&h, "owner left");
    item.payload = json!({ "session_id": "gone" });
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
        true,
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

#[test]
fn concurrent_abstain_and_resolve_leave_exactly_one_terminal_outcome() {
    // Whichever order the flock grants, the CAS arms compose to one terminal
    // truth: abstain-first leaves the item pending for the resolve to win;
    // resolve-first turns the abstain into `NotPending`. Exactly one
    // effective feed.resolve lands either way.
    let h = crate::common::Harness::new();
    let opus: ResolverId = "opus-policy".parse().unwrap();
    let mut item = bridge_permission(&h, "race me");
    item.activate_resolver_chain(vec![chain_step(&opus, 10, 30_000)]);
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resolve = {
        let ledger = h.ledger.clone();
        let request_id = request_id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            ledger.resolve_feed_item(
                &request_id,
                Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::Cli),
                true,
                "rimz-test",
            )
        })
    };
    let abstain = {
        let ledger = h.ledger.clone();
        let request_id = request_id.clone();
        let opus = opus.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            ledger.abstain_feed_item(&request_id, &opus, None, "rimz-test")
        })
    };

    let resolve = resolve
        .join()
        .expect("resolve thread")
        .expect("the override resolve wins in either interleaving");
    assert!(resolve.effective && !resolve.late);

    match abstain.join().expect("abstain thread") {
        // Abstain won the flock: it exhausted the chain and left the item
        // pending for the resolve that followed.
        Ok(outcome) => assert!(outcome.next_resolver.is_none()),
        // Resolve won the flock: the abstain found a terminal item.
        Err(err) => assert!(
            matches!(
                err,
                rimz::ledger::LedgerErr::FeedStore(rimz::ledger::FeedStoreErr::NotPending { .. })
            ),
            "the losing abstain is NotPending, never a second outcome: {err:?}"
        ),
    }

    let after = h.ledger.load_feed_item(&request_id).expect("load");
    assert_eq!(after.status, FeedStatus::Resolved);
    let events = h.ledger.read_events().expect("events");
    let resolves: Vec<_> = events
        .iter()
        .filter(|event| event.method == "feed.resolve")
        .collect();
    assert_eq!(resolves.len(), 1, "exactly one resolve event");
    assert_eq!(resolves[0].params["effective"], true);
}
