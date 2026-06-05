# Ledger

Local contract for `crates/rimz/src/ledger/` — durable workspace state. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules. The durability contract — surfaces, feed lifecycle, CAS rules, the decision bridge — lives in [docs/internals/ledger.md](../../../../docs/internals/ledger.md).

## Write path

- Every mutator lives in [`writer.rs`](./writer.rs) and runs its critical section under the workspace lock — lock → feed-write → event-append; the wakeup and snapshot-publish tail run off-lock. Reads on the `Ledger` handle are lock-free.
- Cross-process serialization is the workspace lock's job: every writer is a short-lived CLI process serialized through `workspace.lock`; there is no in-process actor.
- The two helpers in [`atomic.rs`](./atomic.rs) cover every durable write — `write_temp_then_rename` for whole files, `append_framed_record` (fsync per record) for the event log. No module hand-rolls its own atomic dance.
- The pending/terminal feed split is load-bearing: a terminal write lands beside the pending file, then an atomic rename moves it into `terminal/` — no crash window resurrects a decided ask, and decision-path scans stay O(pending).
- The dead-owner abandon sweep is debounced through a stamp beside the workspace lock, keeping the write path O(1) over feed history; the stamp lives outside the feed dir so item scans never see it.

## Read path

- Snapshot reducers (`snapshot/`) are pure over the event log plus caller-supplied inputs — the `sidebar` CLI supplies the live pane list; nothing under `snapshot/` calls the mux.
- The fold is resumable: the rollup cache and its extent stamp carry over across log rotation, tombstones included — a rebuild never silently drops an ended agent.
- [`single_flight.rs`](./single_flight.rs) owns only the lock-and-poll election and imports no ledger-writer module — it sits inside the sidebar's read-only import graph (CI grep).

## Tests

Durability behaviour — CAS round trips, torn-record recovery, rotation, the write path — lives in [`tests/integration/ledger/`](../../tests/integration/ledger/); pure reducer and helper tests stay in-module. Time is fixed-epoch per [docs/contributing/testing.md](../../../../docs/contributing/testing.md).
