# Message system

Local contract for `crates/rimz/src/message/`: the durable queue that routes text into a running agent's pane. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

Topic detail lives in [messaging.md](../../../../docs/internals/harness/messaging.md), which owns the record, the ordered delivery check, the pipeline, reply waits, channels, and the transcript.

## Invariants

- **The record is the message.** Every send persists a `MessageRecord` before a byte reaches a pane. A pane write is an attempt against that record, never the message itself.
- **`Sent` precedes the submit.** `write_batch` records the batch as `Sent` after the paste lands and before it presses Enter, so a submitted message always has a durable record and audit event behind it.
- **One card, one FIFO queue.** Records key on `(kind, agent_id)` with `agent_name` folding a provisional `launch_*` id into the session it registers as. `msg_` id string order is FIFO order.
- **Acknowledgements follow submitted text.** A submit that contains an intact headered record confirms it. Stray composer text around RimZ's envelope is direct input and is never a reason to write the pane again; headerless system records confirm only on an exact whole-prompt match, and reported text that contains no record confirms nothing. A late acknowledgement stays valid while the record is `Queued`; only a new claim supersedes it.
- **Commands reach the pane at most once.** An unconfirmed command times out without a resend because duplicate commands such as `/compact` can destroy context.
- **Two counters, two caps.** `attempts` guards pre-send claim failures (`MAX_DELIVERY_ATTEMPTS`); for prompts, `unconfirmed_sends` guards writes a lifecycle hook never confirmed (`DEFAULT_MAX_DELIVERY_ATTEMPTS`, overridable per run through the `RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS` environment variable). Keep them separate.
- **Delivery reads store state.** Gates evaluate the rollup and the message queue. Focused-pane state and captured composer contents never decide a delivery.
- **`retry_after` is a wake hint.** It schedules the elder's next look and never affects `is_ready`, FIFO position, claim leases, or hook-driven delivery.

## Boundaries

- Layering runs one way: `dispatch` calls `deliver` and `send`; `deliver` calls `send`; `send` calls the store and the mux. Nothing calls back up.
- `message.rs` stays pure: types, card matching, FIFO and batch selection, threshold and schedule parsing, environment knobs. I/O belongs in the submodules.
- Status transitions belong to the store boundary (`store/writer/queue.rs`), under the workspace lock, each with its audit event. The status enum carries vocabulary, not rules.
- Message content never enters the event log. Terminal text lives in `messages/history.jsonl`.
- `fire.rs` runs on the renderer's cache-refresh tick, so keep it as light as that path demands. It reads the wake stamp and spawns `message sweep`; store reads and writes stay in that helper.
- Address grammar, handle rendering, and channel resolution live in `harness/target.rs`. This module resolves targets through it and never parses addresses itself.
- CLI handlers own flag parsing, rendering, and exit codes. Dispatch conditions, delivery causality, and reply-wait state live here.
