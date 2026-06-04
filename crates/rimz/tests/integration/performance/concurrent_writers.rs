//! History-independence of the ledger write path under concurrency.
//!
//! Tens of agents in one session put a ledger write behind every hook event,
//! all serialized through the workspace flock. The write path's contract:
//! the critical section is feed write + event append only, the dead-owner
//! sweep is debounced, the snapshot publishes off the lock from a resumable
//! fold base, and every per-write scan is O(pending) — so a long session's
//! terminal-feed history costs a writer nothing.
//!
//! Writer concurrency runs as threads over cloned `Ledger` handles: the
//! workspace flock is taken per acquire (fresh fd each time), so threads
//! contend on it exactly like processes, and the burst measures the ledger
//! write path itself rather than binary spawn overhead.

use std::time::{Duration, Instant};

use rimz::ledger::snapshot;
use rimz::{FeedItem, FeedKind, FeedStatus, Surface};

use crate::common::Harness;

const WRITERS: usize = 20;
const PUSHES_EACH: usize = 5;
const HISTORY_ITEMS: usize = 1000;

/// Seed `count` terminal feed files the way a long session leaves them —
/// decided items relocated into `feed/terminal/`. Plain writes: the seed is
/// fixture state, not a durability subject.
fn seed_terminal_history(h: &Harness, count: usize) {
    let terminal_dir = h.ledger.paths().feed_dir.join("terminal");
    std::fs::create_dir_all(&terminal_dir).expect("mkdir terminal");
    for i in 0..count {
        let mut item = FeedItem::new(
            h.workspace_id.clone(),
            Surface::Script,
            FeedKind::Question,
            format!("decided long ago {i}"),
            "rimz",
            "cli",
        );
        item.status = FeedStatus::Resolved;
        std::fs::write(
            terminal_dir.join(format!("{}.json", item.request_id)),
            serde_json::to_vec(&item).expect("serialize"),
        )
        .expect("seed terminal item");
    }
}

/// Run the standard burst — `WRITERS` threads, `PUSHES_EACH` pushes each —
/// and return its wall-clock.
fn timed_burst(h: &Harness) -> Duration {
    let start = Instant::now();
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
                    ledger.push_feed_item(&item, "rimz-perf").expect("push");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer thread");
    }
    start.elapsed()
}

fn assert_burst_landed(h: &Harness, label: &str) {
    let events = h.ledger.read_events().expect("events");
    assert_eq!(
        events.iter().filter(|e| e.method == "feed.push").count(),
        WRITERS * PUSHES_EACH,
        "{label}: every concurrent push lands durably"
    );
    let latest = snapshot::read_fresh_latest(h.ledger.paths())
        .unwrap_or_else(|| panic!("{label}: the published view is current after the last writer"));
    let log_len = std::fs::metadata(&h.ledger.paths().events_log)
        .expect("log meta")
        .len();
    assert_eq!(
        latest.reflects_log.expect("stamped").offset,
        log_len,
        "{label}: the group-committed stamp claims the full log"
    );
}

/// The headline regression guard: the same 20-writer burst costs the same
/// against an empty workspace and against one carrying 1000 terminal feed
/// files. The pre-overhaul write path parsed every feed file ever written —
/// per write, under the lock — so history this size blew the burst up by
/// orders of magnitude; the ratio bound catches any reintroduced O(history)
/// scan while staying far above machine-load noise. The absolute ceiling is
/// the backstop for a uniformly broken write path.
#[test]
fn write_burst_cost_is_independent_of_terminal_history() {
    let fresh = Harness::new();
    let fresh_elapsed = timed_burst(&fresh);
    assert_burst_landed(&fresh, "fresh workspace");

    let seeded = Harness::new();
    seed_terminal_history(&seeded, HISTORY_ITEMS);
    let seeded_elapsed = timed_burst(&seeded);
    assert_burst_landed(&seeded, "seeded workspace");
    assert_eq!(
        seeded.ledger.list_feed_items().expect("audit").len(),
        HISTORY_ITEMS + WRITERS * PUSHES_EACH,
        "the audit read still spans the whole history"
    );

    let ceiling = fresh_elapsed * 3 + Duration::from_millis(500);
    assert!(
        seeded_elapsed <= ceiling,
        "a {HISTORY_ITEMS}-item terminal history must not slow the write burst: \
         fresh {fresh_elapsed:?} vs seeded {seeded_elapsed:?} (ceiling {ceiling:?})"
    );
    assert!(
        seeded_elapsed < Duration::from_secs(20),
        "absolute backstop: {} writes took {seeded_elapsed:?}",
        WRITERS * PUSHES_EACH
    );
}
