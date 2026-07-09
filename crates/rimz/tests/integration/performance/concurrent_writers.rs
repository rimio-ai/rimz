//! History-independence of the store write path under concurrency.
//!
//! Tens of agents in one session put a store write behind every hook event,
//! all serialized through the workspace flock. The write path's contract:
//! the critical section is an event append only, the snapshot publishes off
//! the lock from a resumable fold base, and archive history does not enter
//! the hot path.

use std::time::{Duration, Instant};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::AgentSessionId;
use rimz::store::event::{EventEnvelope, EventKind};

use crate::common::Harness;

const WRITERS: usize = 20;
const EVENTS_EACH: usize = 5;
const HISTORY_EVENTS: usize = 1000;

fn lifecycle(workspace_id: rimz::WorkspaceId, agent_id: &str) -> EventEnvelope {
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::Registered,
    );
    EventEnvelope::agent_lifecycle(
        workspace_id,
        "rimz-perf",
        "claude",
        "SessionStart",
        &observation,
    )
}

/// Seed archived event-log history. Plain writes: the seed is fixture state,
/// not a durability subject.
fn seed_archive_history(h: &Harness, count: usize) {
    std::fs::create_dir_all(&h.store.paths().events_archive_dir).expect("mkdir archive");
    let archive = h
        .store
        .paths()
        .events_archive_dir
        .join("events.000000.jsonl");
    for i in 0..count {
        rimz::store::event_log::append(
            &archive,
            &lifecycle(h.workspace_id.clone(), &format!("history-{i}")),
        )
        .expect("seed archive");
    }
}

fn timed_burst(h: &Harness) -> Duration {
    let start = Instant::now();
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let store = h.store.clone();
            let workspace_id = h.workspace_id.clone();
            std::thread::spawn(move || {
                for i in 0..EVENTS_EACH {
                    store
                        .append_event(&lifecycle(workspace_id.clone(), &format!("writer-{w}-{i}")))
                        .expect("append");
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
    let events = h.store.read_events().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("SessionStart")
                )
            })
            .count(),
        WRITERS * EVENTS_EACH,
        "{label}: every concurrent append lands durably"
    );
    let log_len = std::fs::metadata(&h.store.paths().events_log)
        .expect("log meta")
        .len();
    let snapshot = h
        .store
        .snapshot()
        .unwrap_or_else(|err| panic!("{label}: lock-free read after the burst: {err}"));
    assert_eq!(
        snapshot.reflects_log.expect("stamped").offset,
        log_len,
        "{label}: the reader folds to the log's end"
    );
    let checkpoint: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&h.store.paths().latest_snapshot)
            .unwrap_or_else(|err| panic!("{label}: checkpoint exists: {err}")),
    )
    .expect("checkpoint parses");
    let published_offset = checkpoint["reflects_log"]["offset"]
        .as_u64()
        .expect("stamped");
    assert!(
        log_len - published_offset < 64 * 1024,
        "{label}: the unpublished tail stays under the byte budget"
    );
}

#[test]
fn write_burst_cost_is_independent_of_archived_history() {
    let fresh = Harness::new();
    let fresh_elapsed = timed_burst(&fresh);
    assert_burst_landed(&fresh, "fresh workspace");

    let seeded = Harness::new();
    seed_archive_history(&seeded, HISTORY_EVENTS);
    let seeded_elapsed = timed_burst(&seeded);
    assert_burst_landed(&seeded, "seeded workspace");

    let ceiling = fresh_elapsed * 6 + Duration::from_secs(2);
    assert!(
        seeded_elapsed <= ceiling,
        "a {HISTORY_EVENTS}-event archive must not slow the write burst: \
         fresh {fresh_elapsed:?} vs seeded {seeded_elapsed:?} (ceiling {ceiling:?})"
    );
    assert!(
        seeded_elapsed < Duration::from_secs(20),
        "absolute backstop: {} writes took {seeded_elapsed:?}",
        WRITERS * EVENTS_EACH
    );
}
