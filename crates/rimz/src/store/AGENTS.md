# Store

Local contract for `crates/rimz/src/store/` — durable workspace state. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Subsystem behaviour — on-disk shape, event log, write and read paths, write classes, maintenance — lives in [docs/internals/store.md](../../../../docs/internals/store.md).

Store folds the agent model, so `store` imports `agents` and nothing under `agents` imports `store`; the durable message record, its codec, and its FIFO/claim selection live in `store::message`, and `message/` is delivery that imports them.

## Writes and durability

- Every store mutator and its public intent/outcome vocabulary lands under [`writer.rs`](./writer.rs): the façade owns the ordinary commit and log-boundary primitives, and `writer/` owns the debounce, lifecycle, publish, reset, queue, and reap branches. Supervised-run schema, transitions, and their workspace lock belong to `harness::run`; private `run_store` supplies only the durable codec. Store reset is the deliberate cross-record exception because it cancels active runs before rotating room state. Reads on the `Store` handle stay lock-free; snapshot schema is consumed through its owning module.
- `workspace.lock` is the only cross-process serialization. Every writer is a short-lived CLI process, so a change that needs ordering takes the lock rather than introducing an in-process actor.
- Every durable write routes through [`disk/atomic.rs`](../disk/atomic.rs), and every fsync syscall lives inside it; `cargo xtask invariants` rejects a `sync_all`/`sync_data` call anywhere else. No module hand-rolls its own atomic dance, and the event-log frame encoding stays beside its decoder in [`event_log/frame.rs`](./event_log/frame.rs).
- [`message.rs`](./message.rs) owns the durable record vocabulary and FIFO/claim/batch selection, and [`message/codec.rs`](./message/codec.rs) owns the live queue and history codec. [`writer/queue.rs`](./writer/queue.rs) owns all three message surfaces together: the live `messages.jsonl`, terminal text in the sibling `history.jsonl`, and terminal outcomes in the event log, with status transitions under the workspace lock and their audit events. Archive retention stays with event-log rotation, global orphan-temp collection with GC, and cache-local temp-sibling cleanup with [`disk/atomic.rs`](../disk/atomic.rs).
- The group sync and the checkpoint publish debounce through stamps beside the workspace lock, which is what keeps the write path O(1) over log history. Wakeups fire before the publish: consumers fold the log tail from their own cursor, so checkpoint cadence is latency tuning and never truth.
- The wire is the leaf [`wakeup/`](../wakeup/mod.rs) module below `store`; every sender reaches down to it (this module's write tail, `mux`, `remote_control`, the sidebar graph and its renderer, the CLI), and none reaches another module through it. This module reaches it twice: the post-commit `wake_store_delta` send in [`writer/publish.rs`](./writer/publish.rs), and the heartbeat record GC reads while sweeping expired heartbeats in [`gc/collect.rs`](./gc/collect.rs).

## Reads

- `snapshot/` reducers stay pure over the event log plus caller-supplied inputs. The sidebar's produce module ([`src/sidebar/produce/`](../sidebar/produce/mod.rs)) supplies the live pane list; nothing under `snapshot/` calls the mux.
- The fold stays resumable: carryover plus the rollup cache and its extent stamp preserve continuing and ended rows across rotation, so a rebuild never silently drops launch identity or a resumable session.
- Agent and subagent enrichment scans go through [`sidecar.rs`](./sidecar.rs), which gates each scan on stat.
- [`follow.rs`](./follow.rs) is the read-only lifecycle event-log follower.
- [`disk/single_flight.rs`](../disk/single_flight.rs) stays the lock-and-poll election alone and imports no store-writer module. The sidebar depends on it, and the read-only grep matches files under `src/sidebar/` rather than this one, so the boundary holds only where a change here holds it.

## Tests

Durability behaviour — CAS round trips, torn-record recovery, rotation, the write path — lives in [`tests/integration/store/`](../../tests/integration/store/); pure reducer and helper tests stay in-module. Time is fixed-epoch in fixtures.
