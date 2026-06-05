//! History-independence of the warm rollup fold.
//!
//! Every sidebar tab folds the event log on every wakeup, and tens of agents
//! push tens of events per second. The contract
//! (docs/internals/performance.md): a warm fold reads only the bytes
//! appended since its held base — one frame per event, never the log — so
//! per-tab work per event stays O(frame) while the log grows without bound.
//! Companion to `spending_incremental`, which proves the same shape for the
//! transcript walk.

use rimz::EventEnvelope;
use rimz::ledger::event_log::{self, testkit::bytes_read};
use rimz::ledger::snapshot::RollupCursor;
use serde_json::json;

use crate::common::Harness;

const HISTORY_EVENTS: usize = 3_000;
const FLEET: usize = 30;

fn lifecycle(h: &Harness, i: usize) -> EventEnvelope {
    EventEnvelope::new(
        h.workspace_id.clone(),
        "rimz-perf",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        json!({
            "event_name": "SessionStart",
            "agent_id": format!("agent-{}", i % FLEET),
            "signal": { "signal": "registered" },
        }),
    )
}

#[test]
fn delta_fold_is_o_new_bytes() {
    let h = Harness::new();
    let paths = h.ledger.paths();
    // Seed history through the raw log API: mutator tails would publish per
    // append, and the subject here is the reader's fold alone.
    for i in 0..HISTORY_EVENTS {
        event_log::append(&paths.events_log, &lifecycle(&h, i)).expect("seed event");
    }
    let log_len = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len();

    let mut cursor = RollupCursor::new();
    let cold_before = bytes_read();
    let (cold_extent, _) = cursor.fold(paths).expect("cold fold");
    let cold_bytes = bytes_read() - cold_before;
    assert_eq!(cold_extent.offset, log_len, "the cold fold reaches the end");
    assert_eq!(cold_bytes, log_len, "a cold fold reads the whole history");

    // One event lands; the warm fold pays for that frame alone.
    event_log::append(&paths.events_log, &lifecycle(&h, HISTORY_EVENTS)).expect("append one");
    let appended = std::fs::metadata(&paths.events_log)
        .expect("log meta")
        .len()
        - log_len;

    let warm_before = bytes_read();
    let (warm_extent, agents) = cursor.fold(paths).expect("warm fold");
    let warm_bytes = bytes_read() - warm_before;

    assert_eq!(warm_extent.offset, log_len + appended);
    assert_eq!(
        warm_bytes, appended,
        "a warm fold reads exactly the appended frame, independent of the \
         {cold_bytes}-byte history"
    );
    assert_eq!(agents.len(), FLEET, "the fold still lands the merged view");
}
