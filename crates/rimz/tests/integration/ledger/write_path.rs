//! The 2026-06 write-path contract: a mutation's critical section is feed
//! write + event append only; the snapshot publishes off the lock through
//! `locks/publish.lock` (group commit); readers are lock-free and recover
//! any commit the publisher missed by folding the delta themselves.

use rimz::ledger::{snapshot, AskExpiry};
use rimz::{
    EventEnvelope, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, RuntimeOwner,
    RuntimeOwnerKind, RuntimeScope, Surface,
};
use serde_json::json;

fn native_ask(h: &crate::common::Harness, title: &str, session_id: &str) -> FeedItem {
    let mut item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        title,
        "claude",
        "agent-hook",
    );
    item.payload = json!({ "session_id": session_id });
    item
}

fn lifecycle(h: &crate::common::Harness, event_name: &str, agent_id: &str) -> EventEnvelope {
    EventEnvelope::new(
        h.workspace_id.clone(),
        "rimz-test",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        json!({
            "event_name": event_name,
            "agent_id": agent_id,
            "status": "idle",
        }),
    )
}

/// A pid whose process has already exited and been reaped — `owner_is_live`
/// reads `/proc/<pid>` and finds nothing.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let pid = child.id();
    child.wait().expect("wait true");
    pid
}

fn log_len(h: &crate::common::Harness) -> u64 {
    std::fs::metadata(&h.ledger.paths().events_log)
        .map(|meta| meta.len())
        .unwrap_or(0)
}

#[test]
fn resolve_leaves_latest_reflecting_the_resolve_event() {
    // Stage A regression: the publish must reflect the resolve's own event
    // append, so the very next read is served O(1) instead of re-folding.
    let h = crate::common::Harness::new();
    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Bridge,
        FeedKind::Permission,
        "allow?",
        "claude",
        "agent-hook",
    );
    let request_id = item.request_id.clone();
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");
    h.ledger
        .resolve_feed_item(
            &request_id,
            Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::Cli),
            true,
            "rimz-test",
        )
        .expect("resolve");

    let latest = snapshot::read_fresh_latest(h.ledger.paths())
        .expect("latest.json reflects the full log right after a resolve");
    assert_eq!(
        latest.reflects_log.expect("stamped").offset,
        log_len(&h),
        "the stamp claims exactly the live log"
    );

    // The decided item left the pending scan but stays an audit fact.
    let terminal_file = h
        .ledger
        .paths()
        .feed_dir
        .join("terminal")
        .join(format!("{request_id}.json"));
    assert!(terminal_file.exists(), "terminal item relocated");
    assert_eq!(
        h.ledger.load_feed_item(&request_id).expect("load").status,
        FeedStatus::Resolved
    );
    assert_eq!(
        h.ledger.list_feed_items().expect("audit list").len(),
        1,
        "the audit list spans the partition"
    );
}

#[test]
fn dead_owner_sweep_is_debounced_to_the_interval() {
    let h = crate::common::Harness::new();

    // First write stamps the sweep; then a pending item whose owner is dead.
    h.ledger
        .append_event(&lifecycle(&h, "SessionStart", "a"))
        .expect("stamping write");
    let mut orphan = FeedItem::new(
        h.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        "owner died",
        "rimz",
        "cli",
    );
    orphan.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Script,
        "feed-ask",
        dead_pid(),
        None,
    ));
    let orphan_id = orphan.request_id.clone();
    h.ledger.push_feed_item(&orphan, "rimz-test").expect("push");

    // A write inside the interval does not re-scan: the orphan stays pending.
    h.ledger
        .append_event(&lifecycle(&h, "UserPromptSubmit", "a"))
        .expect("write inside the interval");
    assert_eq!(
        h.ledger.load_feed_item(&orphan_id).expect("load").status,
        FeedStatus::Pending,
        "no sweep inside the debounce window"
    );

    // Age the stamp past the interval; the next write sweeps inline.
    let stamp = h.ledger.paths().locks_dir.join("abandon-sweep.stamp");
    std::fs::File::open(&stamp)
        .expect("sweep stamp exists")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3))
        .expect("age stamp");
    h.ledger
        .append_event(&lifecycle(&h, "Stop", "a"))
        .expect("write past the interval");

    let after = h.ledger.load_feed_item(&orphan_id).expect("load");
    assert_eq!(after.status, FeedStatus::Abandoned);
    assert_eq!(
        after
            .resolution
            .expect("abandon resolution")
            .reason
            .as_deref(),
        Some("owner_process_exited")
    );
    let events = h.ledger.read_events().expect("events");
    assert!(
        events.iter().any(|e| e.method == "feed.abandon"),
        "the sweep appends the durable audit record"
    );
}

#[test]
fn superseding_push_expires_priors_then_pushes_in_one_cycle() {
    let h = crate::common::Harness::new();
    let stale = native_ask(&h, "stale ask", "live");
    h.ledger.push_feed_item(&stale, "rimz-test").expect("push");

    let fresh = native_ask(&h, "fresh ask", "live");
    h.ledger
        .push_feed_item_superseding(&fresh, Some(("claude", "live")), "rimz-test")
        .expect("superseding push");

    assert_eq!(
        h.ledger
            .load_feed_item(&stale.request_id)
            .expect("load")
            .status,
        FeedStatus::Abandoned,
        "a fresh ask supersedes the session's prior native_ui ask"
    );
    assert_eq!(
        h.ledger
            .load_feed_item(&fresh.request_id)
            .expect("load")
            .status,
        FeedStatus::Pending
    );

    // One combined cycle, ordered: the expiry lands before the push it makes
    // way for.
    let events = h.ledger.read_events().expect("events");
    let expire_at = events
        .iter()
        .position(|e| {
            e.method == "feed.expire"
                && e.params.get("reason").and_then(|v| v.as_str()) == Some("agent_moved_on")
        })
        .expect("feed.expire");
    let push_at = events
        .iter()
        .rposition(|e| e.method == "feed.push")
        .expect("feed.push");
    assert!(
        expire_at < push_at,
        "feed.expire precedes the superseding feed.push in the log"
    );

    // The off-lock publish reflects the whole combined cycle.
    let latest = snapshot::read_fresh_latest(h.ledger.paths()).expect("fresh");
    assert_eq!(latest.reflects_log.expect("stamped").offset, log_len(&h));
}

#[test]
fn combined_append_expires_the_sessions_asks_in_the_same_cycle() {
    let h = crate::common::Harness::new();
    let ask = native_ask(&h, "pending at session end", "ending");
    h.ledger.push_feed_item(&ask, "rimz-test").expect("push");

    let expired = h
        .ledger
        .append_event_and_expire(
            &lifecycle(&h, "SessionEnd", "ending"),
            Some(("claude", "ending", AskExpiry::SessionEnded)),
        )
        .expect("append + expire");
    assert_eq!(expired, 1);
    assert_eq!(
        h.ledger
            .load_feed_item(&ask.request_id)
            .expect("load")
            .status,
        FeedStatus::Abandoned
    );
    let events = h.ledger.read_events().expect("events");
    assert!(events.iter().any(|e| e.method == "feed.expire"
        && e.params.get("reason").and_then(|v| v.as_str()) == Some("agent_session_ended")));
}

#[test]
fn concurrent_writers_group_commit_the_newest_state() {
    const WRITERS: usize = 8;
    const PUSHES_EACH: usize = 5;
    let h = crate::common::Harness::new();

    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let ledger = h.ledger.clone();
            let workspace = h.workspace_id.clone();
            std::thread::spawn(move || {
                for i in 0..PUSHES_EACH {
                    let item = FeedItem::new(
                        workspace.clone(),
                        Surface::Script,
                        FeedKind::Question,
                        format!("writer {w} push {i}"),
                        "rimz",
                        "cli",
                    );
                    ledger.push_feed_item(&item, "rimz-test").expect("push");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let events = h.ledger.read_events().expect("events");
    assert_eq!(
        events.iter().filter(|e| e.method == "feed.push").count(),
        WRITERS * PUSHES_EACH,
        "every concurrent push landed durably"
    );
    let latest = snapshot::read_fresh_latest(h.ledger.paths())
        .expect("after the last writer, the published view is current");
    assert_eq!(
        latest.reflects_log.expect("stamped").offset,
        log_len(&h),
        "group commit: the last publisher folded to the log's end"
    );
}

#[test]
fn a_reader_recovers_a_commit_that_never_published() {
    // A writer that crashes between releasing the workspace lock and
    // publishing costs nothing: the commit is durable in the log, the stale
    // stamp declines the fast path, and the reader folds the delta itself.
    let h = crate::common::Harness::new();
    h.ledger
        .append_event(&lifecycle(&h, "SessionStart", "published"))
        .expect("published write");

    // The crashed writer: a bare log append with no publish.
    rimz::ledger::event_log::append(
        &h.ledger.paths().events_log,
        &lifecycle(&h, "SessionStart", "unpublished"),
    )
    .expect("bare append");

    assert!(
        snapshot::read_fresh_latest(h.ledger.paths()).is_none(),
        "the stale stamp declines the fast path"
    );
    let snapshot = h.ledger.snapshot().expect("lock-free read");
    assert_eq!(
        snapshot.reflects_log.expect("stamped").offset,
        log_len(&h),
        "the reader folded the unpublished delta to the log's end"
    );
    let projection = h
        .ledger
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection");
    let ids: Vec<&str> = projection
        .agents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    assert!(
        ids.contains(&"unpublished"),
        "the recovered commit's agent is visible: {ids:?}"
    );
}

#[test]
fn rotation_bumps_the_generation_and_reseeds_the_fold() {
    let h = crate::common::Harness::new();
    h.ledger
        .append_event(&lifecycle(&h, "SessionStart", "before-rotation"))
        .expect("append");

    let outcome = h
        .ledger
        .rotate_event_log(1, None)
        .expect("rotate");
    assert!(outcome.rotation.is_rotated());
    assert_eq!(
        outcome.carryover_agents, 1,
        "the rotating log's rollup moved into the carryover"
    );

    h.ledger
        .append_event(&lifecycle(&h, "SessionStart", "after-rotation"))
        .expect("append into the fresh generation");

    let latest = snapshot::read_fresh_latest(h.ledger.paths()).expect("fresh");
    let extent = latest.reflects_log.expect("stamped");
    assert_eq!(extent.generation, 1, "rotation bumped the generation");
    assert_eq!(extent.offset, log_len(&h));

    let projection = h
        .ledger
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection");
    let mut ids: Vec<&str> = projection
        .agents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["after-rotation", "before-rotation"],
        "carryover and fresh-generation agents both project"
    );
}

#[test]
fn torn_inflight_tail_does_not_drop_a_folded_agent() {
    // The structural guarantee that let reads go lock-free: the fold base
    // already holds every committed event, so racing a writer's half-written
    // tail frame can only delay that one frame — never lose a folded one.
    let h = crate::common::Harness::new();
    h.ledger
        .append_event(&lifecycle(&h, "SessionStart", "folded"))
        .expect("append");
    let committed = log_len(&h);

    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&h.ledger.paths().events_log)
        .expect("open log")
        .write_all(b"512 {\"half\":")
        .expect("write in-flight bytes");

    let snapshot = h.ledger.snapshot().expect("read races the in-flight tail");
    assert_eq!(
        snapshot.reflects_log.expect("stamped").offset,
        committed,
        "the extent stops at the last complete frame"
    );
    let projection = h
        .ledger
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection");
    assert!(
        projection.agents.iter().any(|a| a.agent_id == "folded"),
        "the previously-folded agent survives the race"
    );
}
