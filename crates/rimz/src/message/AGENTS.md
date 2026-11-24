# Message system

Local contract for `crates/rimz/src/message/`: delivery of durable text into a running agent's pane. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

[messaging.md](../../../../docs/internals/harness/messaging.md) owns the mechanics behind every rule here (the record, the ordered delivery check, reconciliation, compaction, wakeups). Read the section linked beside a rule before changing the code it governs.

## Invariants

- **The record is the message.** Every send persists a `MessageRecord` before a byte reaches a pane, and a pane write is an attempt against that record. `write_text` records the batch `Sent` after the text lands and before it presses the agent's submit keys ([the barrier](../../../../docs/internals/harness/messaging.md#the-sent-before-submit-barrier)).
- **One pane, one writer.** Every RimZ pane write (`message`, `answer`, `pane send`) runs inside `mux::PaneWriter` ([Writing to the pane](../../../../docs/internals/harness/messaging.md#writing-to-the-pane)).
- **One card, one FIFO queue.** `msg_` id string order is FIFO order, and a `Sent` prompt keeps its slot until acknowledged or its reconcile deadline passes. The exceptions are an explicit steer or interrupt, the `Resume` control lane, a cache keepalive's forced fire, and the smart-compaction reminder, which is never a FIFO head, batch member, blocker, or held slot and is never requeued or resent ([The FIFO hold](../../../../docs/internals/harness/messaging.md#the-fifo-hold), [Mid-turn reminder](../../../../docs/internals/harness/messaging.md#mid-turn-reminder)).
- **A queued record follows its card.** Each attempt binds the pane its card holds then ([The record](../../../../docs/internals/harness/messaging.md#the-record)).
- **A sent command is never resent.** An unconfirmed `Sent` command times out, because a duplicate `/compact` can destroy context.
- **Expiry has one reader and one writer.** Read-time expiry belongs to `MessageRecord::is_deliverable`, with no helper or reader-local guard beside it, and `Store::reconcile_stale_messages` alone writes `Expired`. Its card-aware rule for a resume follow-up never enters pure record expiry ([Ended receivers](../../../../docs/internals/harness/messaging.md#ended-receivers)).
- **Acknowledgements follow submitted text.** A batch is pasted and aligned in the one order `store::message::paste_order` gives. Composer text around a batch is never a reason to write the pane again ([Confirmation and retry](../../../../docs/internals/harness/messaging.md#confirmation-and-retry)).
- **Holds are silent, and `retry_after` is a wake hint.** A reconciliation hold on a `Sent` record (an open compaction bracket, unapplied hook ingress, a starting receiver) moves no counter and emits no audit event ([Reconciliation](../../../../docs/internals/harness/messaging.md#reconciliation)).
- **Two counters, two caps.** Keep `attempts` and `unconfirmed_sends` separate.
- **Delivery decides from store state.** Captured composer contents never decide a delivery. The team board read (`harness::scratch::board_run`) and the hosting lookup (`launcher::agent_pane::locate`) are the upward dependencies `refactor-target.toml` admits, and a wait on the hosting lookup counts no attempt ([The ordered check](../../../../docs/internals/harness/messaging.md#the-ordered-check)).
- **A pre-claim refusal has named consumers.** A change that adds a pre-claim refusal or a verdict rules what each of these does with it: `deliver::deliver_one` (folds every refusal to `false`), `deliver::sweep`, `message steer <id>` (`cli/message/edit.rs::push_queued_message`), `synthetic::attempt_now`, `soft_compact::send_reminder`, `message show` (`cli/message/show.rs::open_delivery`, through `deliver::explain_before_claim`), and the fresh-send receipt (`cli/send.rs::render_dispatch_outcome`).
- **A claimed batch never fails as a whole on its head's account.** A stop decided from the head alone settles terminally only the records that decision is about and returns every other claimed member to `Queued` with no attempt counted. A change to what may share a batch reads every terminal `record_message_delivery_failures` call in `execute_attempt` against the new members ([Batching](../../../../docs/internals/harness/messaging.md#batching)).
- **A broken premise withdraws, never notifies.** `MessageRecord::broken_row_premise` judges the row-only premises and `premise::broken` adds the harness subjects: a new premise rule goes in one of the two, never at a call site. A withdrawal ends `Canceled` with the broken premise as its reason and sends no sender notice, and an unknown premise kind decodes and holds. A park record closing withdraws nothing itself, since `auto_continue.rs` sits in the sidebar graph ([Message premises](../../../../docs/internals/harness/messaging.md#message-premises)).
- **A notice cannot cause a notice.** Harness senders are never sent a sender notice ([Sender notices](../../../../docs/internals/harness/messaging.md#sender-notices)).

## Boundaries

- Layering runs one way: `dispatch` calls `deliver` and `send`; `deliver` calls `send`; `send` calls the store, the mux, and the hosting lookup. Nothing calls back up.
- The record schema, FIFO/claim/batch selection, and submitted-prompt classification live in `store::message`. Status transitions stay in `store/writer/queue.rs`: the status enum carries vocabulary, not rules.
- Message content never enters the event log. Terminal text under `audit/messages/` is audit.
- `fire.rs` runs on the sidebar host's cache-refresh tick, so keep it as light as that path demands.
- This module resolves targets through the top-level `address` module and never parses addresses itself.
- A change to the lines a delivery pastes (a header, its line count, the paste order) or to the keys it presses finds the live-test stand-ins (`rg -n 'read -r|sleeping_agent_shim' crates/rimz/tests/integration/journey crates/rimz/tests/integration/backend`) and fixes each one: a cooked tty takes Ctrl+S as XOFF, and a `read -r` waits for a newline a non-Enter submit never sends.
- CLI handlers own flag parsing, rendering, and exit codes. Dispatch conditions, delivery causality, and turn-completion reading live here.
