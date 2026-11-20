# Store

Local contract for `crates/rimz/src/store/`: durable workspace state. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

[store.md](../../../../docs/internals/store.md) owns the on-disk shape, the event log, the write and read paths, and maintenance. Its [Where the code lives](../../../../docs/internals/store.md#where-the-code-lives) says which file owns what.

## Writes and durability

- `store` imports `agents`, and nothing under `agents` imports `store`. `store` reaches nothing in `harness`: `harness` is policy over store records (run, workflow, and signal transitions, each under the workspace lock) and reaches down. Disk mechanics sit below in `disk`.
- **Invariant.** Every durable write routes through [`disk/atomic.rs`](../disk/atomic.rs), and every fsync syscall lives inside it: `cargo xtask invariants` rejects a `sync_all` or `sync_data` call anywhere else.
- Lock order is `workspace.lock`, then exclusive `locks/hook-ingress.lock`, never the reverse ([Hook ingress](../../../../docs/internals/store.md#hook-ingress)).
- Reads on the `Store` handle take no lock. The exceptions are `list_messages_and_history` and `store::message`'s `read_status` and `read_record`, which hold the workspace lock across the live queue and the message history.
- A type carried in an event's params grows only by optional, defaulted fields: a new enum variant or a required field makes an older binary drop the whole event from every fold, and worktrees share one store, so mixed binaries are the ordinary case ([what the log holds](../../../../docs/internals/store.md#what-is-in-it)).
- A new lifetime class, or a change to what reset, teardown, or GC removes, updates the class table, [Reset](../../../../docs/internals/store.md#reset), and [What survives what](../../../../docs/internals/store.md#what-survives-what) in store.md, and reset's numbered steps in [maintenance.md](../../../../docs/reference/cli/maintenance.md#reset-a-wedged-room).
- Appends stay O(1) over log history, and checkpoint cadence is latency tuning.
- A message status transition goes through [`writer/queue.rs`](./writer/queue.rs), which owns the live `messages.jsonl`, the terminal text under `audit/messages/`, and the terminal outcome in the event log together, under the workspace lock.
- `classify_submitted_prompt` is read-only over confirmed records and the card's in-flight queue view: it never changes a status. `TurnStarted` confirms prompts and `Compaction` confirms commands.

## Reads

- `snapshot/` reducers stay pure over the event log plus caller-supplied inputs: nothing under `snapshot/` calls the mux.
- `Store::snapshot` leaves `focused_pane` and pane enrichment empty, since the sidebar fuses them in. A decision that needs focus reads the published sidebar snapshot or the mux focus anchor.
- A decision about attention (bucket counts, the pane jump, a projection that overrides a row's status) reads `SidebarRow::attention_status`, not `status()`: a current child's `?` lifts its parent only there.
- A field added to `AgentCard` carries what the renderer reads (a bool or a short label), not the record behind it: the sidebar snapshot is persisted as `latest.json` and projected for every pane.
- A change to the snapshot fold or its reducer times the fold benches, base against head, before hand-off: [Timing comparisons](../../../../docs/contributing/running-gates.md#timing-comparisons).
- Agent and subagent enrichment scans go through [`sidecar.rs`](./sidecar.rs).
- [`follow.rs`](./follow.rs) folds lifecycle records apart from the rollup, so a rule that changes how a record folds is one function both call: `AgentState::snapshot_transition` for state snapshots and `session_death::reaped_end_rests_at_idle` for reaper ends.

## Versions and carried shapes

- A field added to a struct the persisted snapshot carries bumps `SNAPSHOT_VERSION` ([`snapshot/view.rs`](./snapshot/view.rs)); a field the rollup cache carries bumps `ROLLUP_CACHE_VERSION` ([`snapshot/fold.rs`](./snapshot/fold.rs)). An `AgentState` field bumps both. One test pins the current `SNAPSHOT_VERSION`, `*_contract_is_version_<N>` in [`snapshot/assemble/tests.rs`](./snapshot/assemble/tests.rs): a bump renames it rather than adding a second, and the bump's reason goes in its own `pre_<x>_..._is_not_a_fresh_view` test.
- The carryover file is unversioned: neither version reaches it, and an older binary that cannot decode a carried `AgentState` field fails every fold until the file is replaced. A change to a carried field's serialized shape writes the new shape under a new serde key rather than reshaping the old one, and pins that with a decode of the new row through the old struct.
- A change to what the rollup cache or carryover stores keeps `fold_seed_delta_equals_fold_empty_all_over_arbitrary_sequences` in `snapshot/fold/tests/integrity.rs` passing (a warm fold over the cache equals a cold fold from empty): run it before any other fold test.

## Tests

- Fixture time is fixed-epoch, except lifecycle events a test reads back through `Store::snapshot()`: the snapshot reaps pidless rows idle past three hours, so those stamps stay near now.
- A test that needs distinct same-name cards sets `parent_agent_id` and `launch.launch_depth` on each observation, or adoption folds them into one row ([instances.md](../../../../docs/internals/agents/instances.md#agents-sessions-and-epochs)).
- A test that forges a replacement's size or mtime to prove an identity check sees the inode change holds each replaced file open: ext4 hands a freed inode to the next file at once, while the dev host's ZFS does not, so the test passes locally and fails in CI.
