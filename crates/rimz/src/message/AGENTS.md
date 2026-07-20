# Message system

Local contract for `crates/rimz/src/message/`: the durable queue that routes text into a running agent's pane. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

Topic detail lives in [messaging.md](../../../../docs/internals/harness/messaging.md), which owns the record, the ordered delivery check, the pipeline, reply waits, channels, and the transcript.

## Invariants

- **The record is the message.** Every send persists a `MessageRecord` before a byte reaches a pane. A pane write is an attempt against that record, never the message itself.
- **`Sent` precedes the submit.** `write_batch` records the batch as `Sent` after the paste lands and before it presses Enter, so a submitted message always has a durable record and audit event behind it.
- **One card, one FIFO queue.** Records key on `(kind, agent_id)` with `agent_name` folding a provisional `launch_*` id into the session it registers as. `msg_` id string order is FIFO order.
- **Two counters, two caps.** `attempts` guards pre-send claim failures (`MAX_DELIVERY_ATTEMPTS`); `unconfirmed_sends` guards writes a lifecycle hook never confirmed (`RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS`). Keep them separate.
- **Delivery reads store state.** Gates evaluate the rollup and the message queue. Focused-pane state and captured composer contents never decide a delivery.
- **`retry_after` is a wake hint.** It schedules the elder's next look and never affects `is_ready`, FIFO position, claim leases, or hook-driven delivery.

## Boundaries

- Layering runs one way: `dispatch` calls `deliver` and `send`; `deliver` calls `send`; `send` calls the store and the mux. Nothing calls back up.
- `message.rs` stays pure: types, card matching, FIFO and batch selection, threshold and schedule parsing, environment knobs. I/O belongs in the submodules.
- Status transitions belong to the store boundary (`store/writer/queue.rs`), under the workspace lock, each with its audit event. The status enum carries vocabulary, not rules.
- Message content never enters the event log. Terminal text lives in `messages/history.jsonl`.
- `fire.rs` is in the sidebar import graph. It reads the wake stamp and spawns `message sweep`; store reads and writes stay in that helper.
- Address grammar, handle rendering, and channel resolution live in `harness/target.rs`. This module resolves targets through it and never parses addresses itself.
- CLI handlers own flag parsing, rendering, and exit codes. Dispatch conditions, delivery causality, and reply-wait state live here.
