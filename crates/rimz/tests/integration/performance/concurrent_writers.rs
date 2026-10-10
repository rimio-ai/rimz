//! History-independence of the store write path under concurrency.
//!
//! Tens of agents in one session put a store write behind every hook event,
//! all serialized through the workspace flock. The write path's contract:
//! the critical section is an event append only, the snapshot publishes off
//! the lock from a resumable fold base, and archive history does not enter
//! the hot path. The measure is event-log bytes read and written, never time.

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::AgentSessionId;
use rimz::store::event::{EventEnvelope, EventKind};
use rimz::testkit::{bytes_read, bytes_read_under, bytes_written};

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
fn seed_archive_history(h: &Harness, count: usize) -> u64 {
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
    std::fs::metadata(&archive).expect("archive meta").len()
}

/// Event-log bytes one burst moves, taken around the writer threads alone.
struct BurstBytes {
    read: u64,
    archive_read: u64,
    written: u64,
}

fn burst(h: &Harness) -> BurstBytes {
    let archive_dir = &h.store.paths().events_archive_dir;
    let read_before = bytes_read();
    let archive_read_before = bytes_read_under(archive_dir);
    let written_before = bytes_written();
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
    BurstBytes {
        read: bytes_read() - read_before,
        archive_read: bytes_read_under(archive_dir) - archive_read_before,
        written: bytes_written() - written_before,
    }
}

fn assert_burst_landed(h: &Harness, bytes: &BurstBytes, label: &str) {
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
    assert_eq!(
        bytes.written, log_len,
        "{label}: the burst writes the active log and nothing else"
    );
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
    let fresh_bytes = burst(&fresh);
    assert_burst_landed(&fresh, &fresh_bytes, "fresh workspace");

    let seeded = Harness::new();
    let archive_len = seed_archive_history(&seeded, HISTORY_EVENTS);
    let seeded_bytes = burst(&seeded);
    assert_burst_landed(&seeded, &seeded_bytes, "seeded workspace");

    assert_eq!(
        seeded_bytes.archive_read,
        0,
        "a {HISTORY_EVENTS}-event archive ({archive_len} B) must not enter the write path: \
         seeded read {} B ({} B archived) / wrote {} B, fresh read {} B / wrote {} B",
        seeded_bytes.read,
        seeded_bytes.archive_read,
        seeded_bytes.written,
        fresh_bytes.read,
        fresh_bytes.written
    );
}
