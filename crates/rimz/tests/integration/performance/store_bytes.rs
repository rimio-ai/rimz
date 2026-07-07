//! Event-log byte budget for one agent lifecycle turn.
//!
//! The overhead table's durable-write row is bounded by the frame bytes handed
//! to the event log, not by a prose estimate. The write path's fsync discipline
//! is covered in `store_fsync`; this pins the per-turn byte envelope.

use rimz::store::event_log;
use rimz::testkit::bytes_written;

use crate::common::Harness;

const SINGLE_TURN_BYTES_CEILING: u64 = 1024;

#[test]
fn single_lifecycle_turn_writes_under_one_kib() {
    let h = Harness::new();
    let paths = h.store.paths();
    let event = crate::common::lifecycle_event(&h, "rimz-perf", "SessionStart", "agent-0");

    let before = bytes_written();
    event_log::append(&paths.events_log, &event).expect("append lifecycle event");
    let written = bytes_written() - before;

    assert!(written > 0, "the append was counted");
    assert!(
        written <= SINGLE_TURN_BYTES_CEILING,
        "one lifecycle event wrote {written} bytes, above the \
         {SINGLE_TURN_BYTES_CEILING}-byte durable-write budget"
    );
}
