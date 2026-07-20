# Store

Local contract for `crates/rimz/src/store/` — durable workspace state. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules. [docs/internals/store.md](../../../../docs/internals/store.md) explains the subsystem — on-disk shape, event log, write and read paths, write classes — and is the place to send a reader; this file states the rules a change here must hold.

## Write path

- Every mutator lives under [`writer.rs`](./writer.rs): the façade owns the `commit` primitive for lock → store-write → event-append, and `writer/` owns debounce, lifecycle intent suppression and auto-rotation policy, publish, reset, and queue branches. The wakeup, group-fdatasync, and snapshot-publish tail runs once off-lock. Reads on the `Store` handle are lock-free.
- Cross-process serialization is the workspace lock's job: every writer is a short-lived CLI process serialized through `workspace.lock`; there is no in-process actor.
- The helpers in [`atomic.rs`](./atomic.rs) cover every durable write — `write_temp_then_rename` for whole files, `append_record_bytes` (one `write()`, no fsync) for the event log; appended frames become durable through the write tail's debounced group fdatasync and rotation's pre-rename sync. Every fsync syscall lives in `atomic.rs` (CI grep). No module hand-rolls its own atomic dance; the event-log frame encoding lives beside its decoder in [`event_log/frame.rs`](./event_log/frame.rs).
- The message queue ([`writer/queue.rs`](./writer/queue.rs)) owns one live `messages.jsonl` file, records terminal message text in the sibling `history.jsonl`, and records terminal outcomes in the event log; shared age pruning lives in [`atomic.rs`](./atomic.rs).
- The event-log group sync and the checkpoint publish are debounced through stamps beside the workspace lock, keeping the write path O(1) over log history. Wakeups fire before the publish — consumers fold the log tail themselves, so checkpoint cadence is latency tuning, never truth.

## Read path

- Snapshot reducers (`snapshot/`) are pure over the event log plus caller-supplied inputs — the sidebar's produce module ([`src/sidebar/produce/`](../sidebar/produce/mod.rs)) supplies the live pane list; nothing under `snapshot/` calls the mux.
- The fold is resumable: the rollup cache and its extent stamp carry ended rows across log rotation — a rebuild never silently drops a resumable session.
- Agent and subagent enrichment stores share [`sidecar.rs`](./sidecar.rs) for stat-gated sidecar scans.
- [`single_flight.rs`](./single_flight.rs) owns only the lock-and-poll election and imports no store-writer module — it sits inside the sidebar's read-only import graph (CI grep).

## Tests

Durability behaviour — CAS round trips, torn-record recovery, rotation, the write path — lives in [`tests/integration/store/`](../../tests/integration/store/); pure reducer and helper tests stay in-module. Time is fixed-epoch in fixtures.
