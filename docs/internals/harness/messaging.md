# The message system

> How text reaches a running agent: the durable record, the delivery decision, the pane write, the reply wait, and the channel lanes that scope addressing. The code is `crates/rimz/src/message/`. [fleet.md](./fleet.md) maps this area and owns the [address grammar](./fleet.md#the-address) this module resolves through; [transcript.md](./transcript.md) owns the conversation log that confirmed deliveries write and the ask records that hold delivery back. For users, the command is [cli/message.md](../../reference/cli/message.md).

## What the module does

RimZ gives an agent work by typing into its pane. Agents run their stock CLIs in real terminal panes with no API into them, so `rimz message` types on behalf of a human, a script, a loop task, or another agent.

The receiver is usually busy. It can be mid-turn, blocked on a permission prompt, compacting its context, or not yet launched, and typing at the wrong moment either redirects work in flight or leaves text in a composer nobody submits. One rule handles every case:

> **The durable record is the message; the pane write is only an attempt.**

Every send persists a `MessageRecord` before a byte reaches a pane. If the receiver can take the text now, the same call writes it and marks the record `Sent`. If it cannot, the record stays `Queued` and the agent's next turn boundary delivers it, oldest first. A busy agent, a closed and reopened room, a failed multiplexer write, or a crash between claim and send loses nothing, because the text lives in the record.

The rest of the module follows from that rule: how a record decides it is ready, who wakes up to deliver it, how a write is confirmed, and what happens when confirmation never arrives.

## Module layout

| File | Owns |
| --- | --- |
| [`message.rs`](../../../crates/rimz/src/message.rs) | Delivery assembly and parsing: per-recipient record construction, delivery gates, schedule parsing, and the timing knobs. No I/O. |
| [`message/dispatch.rs`](../../../crates/rimz/src/message/dispatch.rs) | One send request end to end: resolve targets, bind conditions, decide park or live, preflight hooks, enqueue, and pace the fan-out. |
| [`message/deliver.rs`](../../../crates/rimz/src/message/deliver.rs) | Readiness: the ordered delivery check, the delivery attempt and its failure recovery, condition evaluation, the sweep, and the wake stamp. |
| [`message/send.rs`](../../../crates/rimz/src/message/send.rs) | The pane write: bracketed paste, the submit barrier, pacing, the compact-first command, and shared wake-stamp maintenance. |
| [`message/compact.rs`](../../../crates/rimz/src/message/compact.rs) | Standalone compaction for the operator verb and idle compaction: repeat refusal and boundary delivery. |
| [`message/reply.rs`](../../../crates/rimz/src/message/reply.rs) | `--wait`: leg state machines, transcript anchoring, cycle detection, join settlement. |
| [`message/fire.rs`](../../../crates/rimz/src/message/fire.rs) | The elder's side of the clock: read the wake stamp and spawn `message sweep`, naming the room by workspace id and mux. |
| [`store/message.rs`](../../../crates/rimz/src/store/message.rs) | The record schema and vocabulary, card matching, FIFO, claim, and batch selection, prompt alignment for [confirmation](#confirmation-and-retry), and the read-only submitted-prompt origin classifier. |
| [`store/message/codec.rs`](../../../crates/rimz/src/store/message/codec.rs) | The JSONL codec for the live queue and terminal history. |
| [`store/writer/queue.rs`](../../../crates/rimz/src/store/writer/queue.rs) | Every status transition, under the workspace lock, with its audit event where one is written. |
| [`cli/message/`](../../../crates/rimz/src/cli/message) | Flag parsing, rendering, and the inbox verbs. No delivery logic. |
| [`cli/hooks/lifecycle/delivery.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/delivery.rs) | The hook side: confirm sent records and spawn the delivery helper at turn boundaries. |

Three neighbours carry pieces this module depends on: [`address.rs`](../../../crates/rimz/src/address.rs) resolves `@handle#channel` addresses and renders handles, [`transcript.rs`](../../../crates/rimz/src/transcript.rs) is the conversation log, and [`channel.rs`](../../../crates/rimz/src/channel.rs) is the named-channel registry.

The layering runs one way: `dispatch` calls `deliver` and `send`, `deliver` calls `send`, and `send` calls the store and the mux. Nothing calls back up.

## The record

A record is keyed on a card, the logical agent identity the [rollup](../agents/model.md#the-rollup) tracks: a kind plus a session id, with the stable `agent_name` as a second key. The name lets a message address an agent before it registers a session. A record queued against a provisional `launch_*` id keeps that id, and `same_card` folds it into the real session's queue once registration lands. Each card has one FIFO queue, with `Resume` control messages in a separate lane.

`msg_` ids are workspace-unique and time-sortable, so string order is FIFO order. A fresh boundary send assigns its id inside its claim commit, after every record live on the card, keeping that order across concurrent senders. The module relies on this everywhere; replacing the id scheme means replacing the ordering with it.

| Field | Purpose |
| --- | --- |
| `message_id` | `msg_` prefixed, time-sortable, FIFO by string order |
| `workspace_id` | Owning workspace |
| `kind`, `agent_id`, `agent_name` | Receiver card; the name folds provisional ids into the registered queue |
| `address` | Receiver handle as resolved at enqueue, rendered by `list` and `show` after the live card is gone |
| `channel` | Receiver lane at enqueue |
| `sender` | `Human`; `Agent { kind, agent_id, name, profile, role, channel }`; `Harness { notice }`, rendered as `@rimz`; `System`, rendered as `rimz`. `Subagent { kind, name }` still decodes but nothing writes it |
| `automated` | Background orchestration traffic; never earns a dollar-budget waiver |
| `reply_wait` | A CLI is blocked on this record's reply ([Reply waits](#reply-waits)) |
| `in_reply_to` | The messages that opened the sender's authoring turn; empty starts a new conversation ([transcript.md § Causality](./transcript.md#causality)) |
| `body` | `Prompt` (pasted) or `Command` (typed, such as `/compact`) |
| `text` | The content |
| `enter` | Submit with Enter after the write |
| `gate` | `Done`, `Any`, or internal `Resume`: which agent statuses release delivery |
| `force` | Deliver past an agent that is holding a question |
| `pane_id` | Pane affinity when known at enqueue; cleared on retry |
| `status` | [Lifecycle state](#status-lifecycle) |
| `not_before` | Scheduled delivery floor |
| `after`, `when` | Cross-agent conditions, each with a durable `met_at` stamp |
| `auto_compact` | Context-fill threshold that fires a `/compact` ahead of the text |
| `compacted_context_tokens` | The context reading a compaction fired on, so a stale gauge cannot fire it twice |
| `batch_id` | Shared by records written in one paste; a turn start without prompt text confirms the batch together |
| `retry_after` | Wake hint set by the sweep; only a compaction deferral on `Sent` extends its read-side hold |
| `attempts`, `last_attempt_at` | Claim bookkeeping; the cap ends in `Abandoned` |
| `last_sent_at` | Last pane write; survives a prompt requeue so a late acknowledgement can still settle it |
| `unconfirmed_sends` | Prompt writes that reached a pane and were never confirmed; the cap ends in `TimedOut` |
| `last_error` | The most recent delivery or reconciliation failure, the verdict a refused synthetic boundary attempt recorded, or the blocker a `NoPane` back-off recorded |
| `enqueued_at`, `updated_at`, `delivered_at` | Timestamps |

`MessageSender::is_conversation()` splits senders by variant alone. `Human` and `Agent` are conversation traffic; `Harness`, `Subagent`, and `System` are system traffic, which covers nudges, compaction commands, and deliberately unattributed `--no-from` text. The split is a read-side filter for `message list` and other conversation surfaces, and `automated` plays no part in it.

Two counters track two different failures. `attempts` counts claims and caps at `MAX_DELIVERY_ATTEMPTS` (5). `unconfirmed_sends` counts prompt writes that landed and were never acknowledged, and caps at `DEFAULT_MAX_DELIVERY_ATTEMPTS` (3, overridable through `RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS`). A claim bumps only the first; a stale-`Sent` prompt requeue bumps only the second. Commands have no unconfirmed-send cap because they are never resent.

### Turn-opening injection sites

These are the producers of queued or launch-time text. Paths are relative to `crates/rimz/src`; sender and body names are `MessageSender` and `MessageBody` variants. A launch prompt is not a queued `Prompt` record.

| Site | Module and composing symbol | Sender / body queued |
| --- | --- | --- |
| Human or agent message; `--no-from` | `cli/message/dispatch.rs::send_message`, caller text; `cli/send.rs::sender_for` resolves identity | `Human` or `Agent` / `Prompt`; `--no-from` uses `System` / `Prompt` |
| Subagent fleet digest | `cli/agents_cmd/subagent_report.rs::report_fleet_with_kind`, text from `compose_digest` | `Harness { notice: SubagentReport }` / `Prompt` |
| Team report | `cli/agents_cmd/team_report.rs::report_team` | `Harness { notice: TeamReport }` / `Prompt` |
| Child parked on a provider limit | `cli/agents_cmd/park_notice.rs::run`, text from `harness/park_notice.rs::text`, via `message/synthetic.rs::SyntheticMessage` | `Harness { notice: SubagentPaused }` / `Prompt` |
| Silent launched child | `cli/agents_cmd/stall_notice.rs::run`, text from `harness/stall_notice.rs::text`, via `message/synthetic.rs::SyntheticMessage` | `Harness { notice: SubagentStalled }` / `Prompt` |
| Agent-sent message ended undelivered | `crates/rimz/src/store/writer/queue.rs::QueueTxn::close` collects endings; `crates/rimz/src/store/writer/queue.rs::Store::commit_queue` composes through `crates/rimz/src/store/message/sender_notice.rs::undelivered` | `Harness { notice: MessageUndelivered }` / `Prompt` |
| Agent-sent message still queued | `crates/rimz/src/message/deliver.rs::sweep`, text from `crates/rimz/src/store/message/sender_notice.rs::still_queued` | `Harness { notice: MessageQueued }` / `Prompt` |
| Prompt-cache keepalive | `cli/agents_cmd/cache_keepalive.rs::run`, text from `harness/cache_keepalive.rs::prompt`, via `message/synthetic.rs::deliver_now` | `Harness { notice: CacheKeepalive }` / `Prompt` |
| Auto-continue | `cli/agents_cmd/auto_continue.rs::run_auto_continue`, configured resume text via `message/synthetic.rs::SyntheticMessage` and `deliver_now` (or `attempt_now` for a queued id) | `System` / `Prompt` |
| Supervised verification reprompt | `harness/prompt_compose.rs::verify_reprompt`, delivered by `cli/supervised/verify.rs::deliver_reprompt` via `message/synthetic.rs::SyntheticMessage` and `deliver_now` | `System` / `Prompt` |
| Fleet or account budget continuation | `cli/budget.rs::run`, configured resume text via `message/synthetic.rs::SyntheticMessage` | `System` / `Prompt` |
| Agent budget continuation | `cli/agents_cmd/budget.rs::run_budget`, configured resume text via `message/synthetic.rs::SyntheticMessage` | `System` / `Prompt` |
| Idle compaction | `cli/agents_cmd/idle_compact.rs::run_idle_compact`, `agents::compact_command`, `message/compact.rs::send_compact` | `System` / `Command` |
| Stage-flip compaction | `harness/team_stage.rs::compact_flipper`, `agents::compact_command`, `message/compact.rs::send_compact` | `System` / `Command` |
| Operator compaction | `cli/agents_cmd/compact.rs::compact_agent`, adapter compact command, `message/compact.rs::send_compact` | Operator's `Human` or `Agent` / `Command` |
| Smart compact before delivery | `message/send.rs::compact_message_for_target`, `agents::compact_command` | `System` / `Command` |
| Stage notice | `harness/team_stage.rs::stage_open_body`, queued by `open_stage` from a flip, the exec wrapper's resume (`rewake_resumed`), or the registration backstop ([teams.md § Resume re-wake](./teams.md#resume-re-wake)) | `Harness { notice: Stage }` / `Prompt` |
| Loop delivery or self-wait | `harness/schedule/runner.rs::resolve_effect_prompt`, `runner/prompt.rs::compose_wait`; dispatched by `cli/loop_cmd/run.rs::execute_prepared_delivery` | `Harness { notice: Wait }` / `Prompt` |
| Signal or watch delivery | Same scheduler composition and dispatch; team bindings are armed by `harness/schedule/team.rs::arm_member` | `Harness { notice: Signal }` / `Prompt` |
| Loop-spawned run | `harness/schedule/runner.rs::compile_spawn_request` and `resolve_effect_prompt` | No queue record; `RunRecord.loop_task` supplies harness origin |
| Supervised launch or subagent brief | `cli/supervised/run.rs::supervised_prompt`; fallback reminder from `harness/launch_reminders.rs::subagent_reminder` | No queue record; run origin is human or the subagent's parent |
| Supervised retry | `harness/prompt_compose.rs::retry_prompt` | No queue record; retains the run's origin |
| `message --create` on a missing recipient | `cli/message/dispatch.rs::recipient_miss`, caller text passed as a launch prompt | No queue record; agent-sender preservation is deferred |

Run-origin matching and the wrapper registry are described in [transcript.md § Writing entries](./transcript.md#writing-entries). Direct typing and `rimz pane send` have no message record: `pane send` is the raw keystroke primitive and its caller is the author. Ordinary root launch text is likewise not queued. Team system prompts use the system-text channel rather than the conversation; restart, fork, resume, and rebirth inject no turn-opening text. Provider control payloads are filtered through `SanitizedPrompt`, including for process plugins, before they can become lifecycle prompts.

## Status lifecycle

```text
Queued ──► Claimed ──► Sent ──► Delivered
   │          │          │
   │          │          └──► TimedOut    (unconfirmed)
   │          ├──► Queued     (retryable pre-send failure)
   │          ├──► Abandoned  (claim cap)
   │          └──► Errored    (repeat compaction, or a send failure with no retry path)
   │
   ├──► Canceled   (user, or a joined digest)
   ├──► Expired    (automatic command's queueing window elapsed)
   ├──► Archived   (receiver or watched session ended, channel torn down)
   └──► Errored    (no receiver, or a send failure with no retry path)
```

`Queued` and `Claimed` are open states (`is_open()`). `Claimed` is a short lease taken immediately before a write so a concurrent deliverer cannot double-send. A claim older than `CLAIM_TTL` (15 s) counts as expired, so a crash mid-send leaves a record that the next sweep requeues and redelivers through the normal FIFO path.

Every attempt, including a fresh send and a compact-first command, holds a claim before writing and attempts to register its recovery wake immediately after claiming. If wake registration fails, it warns and continues the send: wakeups improve recovery latency, never durable delivery truth. A fresh boundary send checks its card's lane in the same commit that enqueues and claims its record: a deliverable queued non-`Resume` record, an unexpired non-`Resume` claim, or an unacknowledged non-`Resume` `Sent` prompt still inside its hold deadline makes it park `Queued` behind that blocker instead. Dispatch's earlier queue read is only a fast path. Under the pane lock, before pacing or writing any bytes, the sender rechecks that every batch member is still held; if any claim was lost, it writes nothing, releases the remaining holds, and stops without settling a failure. Retrying, erroring, or releasing a record takes the record the sender was handed, and the writer applies the give-up only while the live record is `Claimed` and its `attempts` and `last_attempt_at` still match (the hold). A retry clears the stamp, making the record claimable immediately; an unclaimed record is held by nobody. A sender that has no such record (the boundary helper in `synthetic.rs`) can only settle a `Queued` record and leaves a `Claimed` one alone. Recording `Sent` needs no hold, because the text is already in the pane, but it only marks a live record: a record that a cancel, clear, or archive already finalized stays in that terminal state and is not recreated. A give-up on a record that is no longer live writes nothing, preserving its single terminal history entry and event.

`Sent` means bytes reached the pane. The record stays live until a lifecycle hook confirms it or the reconciler gives up, because a write does not prove the agent took the text. A `Sent` prompt is not an open state, but still holds the card's boundary in its delivery lane until acknowledged or its reconcile deadline passes, including any compaction deferral ([FIFO hold](#the-ordered-check)).

`Delivered` means the agent acknowledged: `TurnStarted` for a `Prompt`, `Compacting` for a `Command`. Neither event confirms the other body.

The seven terminal states are final. A terminal transition removes the record from `records/messages/messages.jsonl`, appends the full record to `audit/messages/<bucket-start>.jsonl`, and appends a `message.*` audit event without text ([Storage and audit](#storage-and-audit)).

| Trigger | Terminal status |
| --- | --- |
| A lifecycle hook correlates with the submitted record | `Delivered` |
| A queued automatic command reaches its validity deadline | `Expired` |
| An unconfirmed command reaches its delivery deadline | `TimedOut` |
| An unconfirmed prompt reaches the unconfirmed-send cap | `TimedOut` |
| The address resolved to no agent, after the durable fallback | `Errored` |
| A send failure with no retry path: a live pane target with no durable card, a failed compact-first command write, or a fresh steer rejected because the agent is waiting on input | `Errored` |
| A queued compaction command would follow an unprompted compaction ([Manual compaction](#manual-compaction)) | `Errored` |
| `message cancel`, `message clear`, or the [join guard](#the-subagent-digest-join-guard) | `Canceled` |
| A retryable failure after `attempts` reached its cap | `Abandoned` |
| The receiver session ended, a watched `when` session ended, or the channel was torn down | `Archived` |

`Archived` records a conversation that no longer exists. `message list` hides archived records; `--all` and `message show` keep them readable. Lifecycle `Ended` archives in realtime, and `rimz gc` is the durable backstop.

## Sending

### Caller identity

The caller resolver decides whether an invocation is agent-authored before dispatch builds its `MessageSender`. It first reads the stable RimZ launch identity from the inherited environment. Without one, it walks the invoking process's ancestry and matches each ancestor's PID and process-start token against a live durable agent `RuntimeOwner`. The nearest match supplies that agent's durable card, even for a provider RimZ did not launch, preferring the current pane when several rows share the owner. No match means a human sender.

The agent sender's optional `agent_id` uses the same identity as `LaunchedBy.agent_id`: the caller's launch id, or its session id for a legacy row without a launch id. Legacy environment callers resolve their unambiguous pane before stamping it. Old message records without this field still decode and omit it on rewrite; names do not supply a missing identity. The field does not change the rendered message header.

Daemon-routed sessions, such as a user-run `codex` or a phone-started background session, stay unattributed on the ancestry path: their tool commands run below the shared app-server, whose `RuntimeOwner` cannot say which agent invoked the command. RimZ-managed Codex launches instead run embedded through `--no-daemon`, with pane-owned hooks ([Codex registration](../agents/adapter_codex.md#session-registration-and-launch-quirks)).

### Four modes on one timing axis

Every mode resolves targets through the same parser, writes the same record shape, uses the same pane write, and emits the same audit events. The modes differ only in when the record may deliver.

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Boundary | default | Write now if the receiver can take it, otherwise park for the next qualifying turn boundary. |
| Steer | `--steer` | Writes into the live turn now, waiting for any in-flight pane write. Conflicts with `--schedule` and `--on`. |
| Interrupt | `--interrupt` | Paste and submit as steer does, then press the declared interrupt key once after `RIMZ_MESSAGE_INTERRUPT_DELAY_MS` (default 3000); the provider stops the turn and submits its queued prompt as a fresh turn. Claude and Codex declare Escape; other kinds are refused. Conflicts with every other timing mode and with `--after` and `--when`. |
| Schedule | `--schedule <DUR\|HH:MM>` | Always park, with a `not_before` floor. |

Steer still writes a `Queued` record first and moves it to `Sent` when the paste lands. When the address resolves only to a durable card with no live pane, steer parks, prints `queued for @handle (msg_...)`, and the retry path delivers once a pane appears.

Internal callers pick a mode too. Self-only timer and watched-command waits use steer; scheduled loop and signal deliveries, team bindings included, use boundary with the `Done` gate. `rimz message @me` resolves the calling agent to its pinned session through the shared CLI resolver and refuses an unidentified, provisional, or ended caller.

### Conditions

Two flags add cross-agent conditions to boundary and schedule sends, and they compose with each other:

- `--after <ADDR>` holds until that agent finishes its queued work. Each address must resolve to exactly one durable card, and repeats form an all-of set. The recipient itself and fan-out addresses are rejected.
- `--when '@handle <status> <duration>'` holds until one agent stays continuously in a raw lifecycle status for the dwell. Self-reference is allowed, which is what makes keep-warm messages (`@codex --when '@codex idle 58m'`) work.

Delivery gates and `when` conditions read status differently, on purpose. Gates read `effective_status()`, which projects budget parks to `Paused`, settles hookless turns, and reads a clean turn parked on background work as `Success`. `when` conditions read the stored `status` field, so a projection never trips a dwell the event log cannot justify.

### The dispatch walk

[`dispatch()`](../../../crates/rimz/src/message/dispatch.rs) is the one entry point for an owned send.

1. **Load what the decision needs.** Pending records (boundary mode), plus agent-context sidecars when a smart-compact threshold, a condition, an agent sender, or `--wait` needs them.
2. **Choose a snapshot.** A full resolution snapshot costs a multiplexer round trip. When every target parks without a live pane (no lazy-registering kind, no provisional id), `targets_all_park_without_live` proves the cached rollup is enough and dispatch skips the mux. This `rollup_only` path is a latency optimization; correctness never depends on it.
3. **Resolve targets.** Live agents and live panes combine, so an agent with a bound pane yields one target. When the live view finds nothing, the durable audit rollup is the fallback, with pane-shadowed co-resident sessions filtered out first. A multi-match without `--all` or `@all` is an ambiguity error listing the candidates. For intrinsic `@all`, dispatch removes an agent caller from the resolved set before anything else runs; no remaining peer is an error.
4. **Bind conditions.** Each `after` and `when` address resolves once and pins a card. A condition already satisfied gets `met_at` stamped immediately, so upstream work must be queued before the message that waits on it.
5. **Decide park or live, per target.** See below.
6. **Deliver, per target in order.** A parked target first passes the hook preflight; then every target gets its durable record, live targets go straight into a delivery attempt, and parked ones stop at `Queued`.
7. **Rearm the wake stamp** so the elder knows when to look again. The rearm is best-effort: a stamp that cannot be written warns on stderr and never changes the send's reported outcome.

The park-or-live decision (`dispatch_decision`) takes the first rule that applies:

1. Steer and interrupt go live when a pane is bound and park otherwise. Interrupt preflights every target before enqueue: a missing key, a live pane without a durable bound session, or an awaiting-input row without `--force` refuses the send, including the whole fan-out. Interrupt always loads agent-context sidecars for that check.
2. A schedule or an unmet condition parks.
3. A receiver that cannot take a prompt parks with a reason: its effective status, or its native-input wait. Readiness comes from the exact pane binding when there is one and from the durable card otherwise.
4. No resolved pane parks.
5. A ready queued record, an unexpired non-`Resume` claim, or an unacknowledged non-`Resume` `Sent` prompt for the same card parks, so a fresh send never jumps the queue or an in-flight write. Expired claims and the `Resume` lane do not block.
6. Otherwise the target goes live.

The hook preflight in step 6 exists because turn-end hooks are what release parked text: a parked record for a kind without installed, trusted hooks would never deliver, so dispatch refuses it. The preflight runs once per login key (kind plus login account) inside the per-target loop, so in a fan-out a later target's preflight failure returns an error after earlier targets were already queued or sent.

Receipts follow the decision. A live write prints `sent to @handle (msg_...)`: the record is `Sent`, and only the receiver's acknowledgment makes it `Delivered`. Status and provider-starting parks retain their steer hints. A native-input wait points to `rimz asks show` and `rimz answer`, not a prompt override. Other parks carry the scheduled opening, first unmet `after` or `when` condition, blocking record id, or missing pane. The schedule wins over `after`, which wins over `when`; readiness still wins over FIFO in dispatch. Only a live attempt that defers without a cause keeps the bare queued receipt. Interrupt retains its delivery-deferred hint. On the rollup-only path the receipt does not prove the pane is still live; `message show` runs the full check.

Fan-out delivers each target in turn, paced one message interval apart, and prefixes each delivery with the addressed handle (`@all,`) so receivers read it as a group message, even when caller exclusion leaves one peer. A member whose attempt is skipped does not stop the rest, and the summary names sent and skipped agents with their message ids.

An address that matches nothing, after the durable fallback, writes a terminal `message.errored` bounce carrying the raw address, so `message list --all` shows the failed hand-off.

## Delivery

### The ordered check

[`DeliveryCheck`](../../../crates/rimz/src/message/deliver.rs) evaluates a queued record against a fresh snapshot and reports the first blocker. The order is the contract: `rimz message show` renders it, the sweep backs off on it, and the delivery helper re-runs it before claiming.

| # | Check | Verdict when it fails | What releases it |
| --- | --- | --- | --- |
| 1 | A queued automatic command is still within its validity window | `Expired` | Nothing; the next sweep records the terminal outcome |
| 2 | `not_before` has passed | `Scheduled` | The clock, via the elder sweep |
| 3 | Every `after` condition stamped | `WaitingOnAfter` | The referenced agent reaching its gate with no ready queued work |
| 4 | Every `when` condition stamped | `WaitingOnWhen` | The watched agent completing its dwell in the raw status |
| 5 | Oldest holding record for this card and lane: a deliverable `Queued` or `Claimed` record, or a `Sent` prompt still inside its reconcile deadline (including compaction deferral) | `BehindFifo` | The blocking record settling, or the prompt's acknowledgement or hold deadline passing |
| 6 | The receiver card exists in the snapshot | `ReceiverGone` | The agent reappearing, or GC archiving the record |
| 7 | The receiver has not ended | `ReceiverEnded` | The sweep archives open records for the card and unmet `when` conditions watching it |
| 8 | Not inside the compaction window | `Compacting` | `CompactionEnded`, or the 90 s window expiring |
| 9 | The gate is open for the effective status | `GateClosed` | The agent reaching `Idle`, `Success`, or `Sleeping`, plus `Failed` under `--on any` |
| 10 | The resumed provider has started | `ProviderStarting` | Its next lifecycle observation, normally `Registered`, or one delivery window elapsing; lazy-registering providers skip this check |
| 11 | A `Resume` gate's park is recoverable | `ResumeUnrecovered` | The budget window resetting or the overload marker clearing |
| 12 | No open blocking prompt reserving input | `AskWaiting` | Answering the ask, or `--force` |
| 13 | A live pane can receive a paste | `NoPane` | A pane appearing; affinity is cleared so any bound pane will do |

All thirteen pass and the verdict is `Ready`. An `Ended` receiver keeps refusing delivery until a later lifecycle fold revives its card: pane attach is not one, and the exec wrapper's `rimz.agent-resumed` stamp on every resume is. A record queued to an ended card before its resume, such as a cohort resume's positional prompt, waits for that stamp. Ended launched children remain in runtime snapshots while their parent is visible. For ended receivers omitted by that projection, the sweep and `message show` consult the audit row rather than treating its absence as an unknown receiver.

A pane is bindable when it reaches `agent_panes`, which the snapshot builds from the panes card admission keeps ([sidebar.md § Presence model](../sidebar/sidebar.md#presence-model)). A pane the fold drops is a receiver no message can reach, however healthy the agent's record looks. Without an end stamp, `NoPane` can outlive every other gate; a durable end instead lets the sweep archive its open records.

The compaction window at check 8 closes every gate, `Resume` included. A receiver with a `compacting_since` marker less than 90 seconds old takes nothing. The window expires so a lost compaction-end signal costs a delay instead of a wedged queue. Stale-`Sent` reconciliation uses the full compaction bracket instead ([model.md § The compaction bracket](../agents/model.md#the-compaction-bracket)).

`DeliveryGate::Resume` has no flag. Auto-continue stamps it on its own nudge, and check 11 re-verifies at delivery time that the park is still resumable. `Done` and `Any` records stay parked while an agent is paused, so a rate-limited agent does not receive a pile of user text the moment it waits. Both open for `Sleeping`, so a resting agent with an armed one-shot wait can receive a message without consuming that wait; `Resume` does not open for `Sleeping`.

Scheduled, condition-blocked, and `Resume`-gated records are left out of the FIFO scan, so they never block a later record that could deliver now. Resume nudges also live in their own lane, so a wakeup never queues behind user text that cannot deliver until after it.

A `Sent` prompt keeps its FIFO slot while its turn-start acknowledgement is pending and its reconcile deadline has not passed, even if the card still reads `Idle`: the store already knows a prompt was submitted before the hook commits. The selector and explain scan share this hold with `Queued` and `Claimed` records, using the same card, lane, and deliverability checks (`crates/rimz/src/store/message.rs::delivery_batch_indices`, `crates/rimz/src/store/message.rs::older_ready_blocker`). Readers release an overdue hold without a sweep or a record transition, through `crates/rimz/src/store/message.rs::MessageRecord::sent_hold_expired`; a compaction deferral extends that deadline. Reconciliation still either requeues the prompt as the older head or times it out. Late acknowledgements remain valid while it is `Sent` or awaiting a late acknowledgement in `Queued`. A `Sent` command does not hold the card: the provider's composer queues a paste during compaction, and holding for the command's 180 s acknowledgement window would freeze prompts when its hook never arrives. Steer and interrupt bypass FIFO, including the prompt hold.

For `ReceiverGone`, `rimz message show` consults the audit rollup. When the durable card survives but runtime projection expelled it, the verdict names the card's last-seen time, says no live process claims it, and points at `rimz agents resume`; the verdict and its JSON name are the same either way.

### What triggers a delivery

Three paths converge on the same helper.

**Lifecycle hooks.** The lifecycle reactor declares [`DELIVERY_CHECKPOINT`](../../../crates/rimz/src/agents/lifecycle/event.rs) as `Registered`, `TurnEnded`, `TurnInterrupted`, and `CompactionEnded`. On one of those it finds the FIFO head for the event's card and spawns a detached `rimz message deliver --message-id <id>`. Registration releases messages parked while a resumed provider starts; the helper still settles and re-checks the gates before claiming. Subagent stops and compaction starts do not check the queue.

The same reactor nudges the sweep when the event's agent is referenced by an unmet condition: `after` conditions on `DELIVERY_CHECKPOINT`, and `when` conditions on the wider [`CONDITION_CHECKPOINT`](../../../crates/rimz/src/agents/lifecycle/event.rs), which adds `TurnStarted`, `AwaitingInput`, and the subagent edges because a dwell can start or break on any of them. Both actions run after the `LifecycleEvent` commits, and the helper re-checks durable state before claiming.

**The elder sweep.** The room's elected sidebar elder spawns `rimz --mux <mux> message sweep --workspace-id <id>` when the wake stamp comes due ([Scheduling and wakeups](#scheduling-and-wakeups)). Its argv names the room and mux even though the host inherits no pane environment.

**Auto-continue.** When a persisted park reaches its reset or backoff condition, the producer spawns `rimz agents auto-continue`, which queues a `Resume` message (or reuses the existing one) and calls the same helper ([providers.md § Auto-continue](../agents/providers.md#auto-continue)).

### The delivery helper

`DeliveryPolicy::Interrupt { force }` shares steer's single-record claim, gate override, and retry handling; the only difference is the key after the write. The CLI offers `rimz message interrupt <id>`. The record stores no delivery mode, so a parked interrupt is delivered at the next checkpoint as an ordinary boundary send, without the key.

`rimz message deliver --message-id <id>` is a hidden helper, never run by hand.

1. **Settle.** Sleep 400 ms (`RIMZ_MESSAGE_SETTLE_MS`) so the agent's state stabilizes after the hook.
2. **Re-check.** Run the ordered check against a fresh snapshot. The hook's choice is a hint; this check decides.
3. **Claim.** Under the workspace lock, refuse a head behind an older deliverable holding record in its card's lane (`Queued`, `Claimed`, or a `Sent` prompt whose hold deadline has not passed). Otherwise move the compatible FIFO [batch](#batching) from `Queued` to `Claimed` and bump each `attempts` in one transaction.
4. **Write.** Send through the [pane write](#writing-to-the-pane). If [smart compaction](#smart-compaction) fires, type the compact command alone and release the prompt batch back to `Queued` without an attempt penalty.
5. **Record.** A successful write moves the batch to `Sent`, live until confirmed.

`rimz message steer <id>` runs the same helper with a steer policy. It still needs the named record, a receiver card, and a live pane, but ignores `not_before`, FIFO position, the gate, and the resume-recovery check. `claim_message_for_steer` claims only the named record, keeping the claim TTL guard and skipping the FIFO compare. It is the manual escape hatch for a dependency cycle or a vanished upstream agent. A waiting ask still defers it unless the record or the command carries `--force`.

A failure before any byte is written keeps or returns the record to `Queued` with `last_error` set and pane affinity cleared, so the next boundary re-resolves a pane. When `attempts` has reached its cap the record becomes `Abandoned` instead, and the terminal failures in the [trigger table](#status-lifecycle) become `Errored`.

### The subagent-digest join guard

A fleet digest (`SUBAGENT_REPORT` or `AGENT_REPORT`) points a launcher at the response files of runs that have settled. A launcher that already joined those runs inline has read the results, so the digest would cost it a turn and show it nothing. Two layers cancel it.

The producer cancels first, as eager cleanup. An attended inline `rimz subagents wait` stamps `joined_at` on the answer it prints; `rimz subagents stop` stamps every answer of the selected run. They cancel each digest whose every listed answer is joined, and the composer rechecks after queueing ([subagents.md § The lifecycle, end to end](./subagents.md#the-lifecycle-end-to-end)). Any of these cancels can fail after the stamp and before the queue changes, so delivery cannot rely on them.

Delivery is the guarantee. [`attempt_delivery`](../../../crates/rimz/src/message/deliver.rs) calls `cancel_joined_subagent_report` before it claims and again after a successful claim, before sending. For a record whose sender is `Harness { notice: SubagentReport }`, each call runs `harness::run::report::digest_fully_joined` over current and earlier answers whose `report_message_id` is this digest. Reopening a child preserves that link on its unjoined earlier answer:

> **A digest with at least one linked answer, every listed answer joined, never reaches the pane.**

When either scan finds that, the guard cancels the digest with reason `joined before delivery` and skips the write. It skips the write even when producer cleanup already removed the record and the cancel finds nothing to cancel. A digest with some rows unjoined, or with no linked rows, delivers its full text, so a genuinely unread result still reaches the parent. Other harness notices take ordinary delivery.

A guard that cannot answer never sends. When the run scan or the cancel fails, the helper warns with the message id. Before the claim it simply stops. After the claim it calls `release_message_claims` to return the digest to `Queued` without an attempt penalty, refreshes the wake stamp, and stops; an error from the release propagates, but an unreadable run file does not abort the sweep. The queued digest keeps its FIFO head position on its card, like any head that cannot deliver.

One race remains. The post-claim scan closes the window where stale pre-claim state survives a claim, but a join after that scan can still race the send, because the store's workspace lock does not span pane I/O (the per-pane write lock does, and it serializes writers, not store transitions). The join-side cancel accepts a `Queued` or `Claimed` record and leaves a `Sent` one for confirmation, since a cancel cannot retract a paste. A cancel that lands while the paste is being written stands: the sender's `Sent` mark finds no live record, warns with the message id, and the record stays `Canceled`. When the guard cancels a record named by `message steer`, the command exits 1 with `message <id> is no longer queued`.

### Batching

When a queued head delivers, the helper extends the claim through the contiguous ready prefix of the head's lane, so messages that piled up during one turn arrive as one interaction.

A record joins the batch only if it:

- is a `Prompt` that submits with Enter and does not start with `/`;
- has its own gate open;
- matches the head's `force` flag;
- shares the head's batch key: the sender's channel for an `Agent`, the receiver's channel for a `Human`, `Subagent`, or `System` sender, and no key for a `Harness` sender.

The first record that fails a rule ends the batch. `Resume` control messages never batch.

A fleet digest (`SUBAGENT_REPORT` or `AGENT_REPORT`) is always claimed alone, as its own head. Nothing else would stop it: a digest is an Enter-submitted `Prompt` with no batch key, so it could ride behind a channel-less human message, and a batch member joins a claim without passing the [join guard](#the-subagent-digest-join-guard). Other harness notices batch normally, including `TEAM_REPORT`.

The batch lands as one paste and one submit. Agent- and human-authored members keep their own [header](#the-message-header), system members stay verbatim, and a blank line separates sections. Claim, `Sent`, release, and pre-send failure each change the whole batch in one queue transaction.

### Confirmation and retry

Origin classification is a separate, read-only decision in `store::message::classify_submitted_prompt`. Before confirmation removes records from the live queue, the hook reads `Store::list_messages` for this card's `Sent` or `awaiting_late_ack()` records, of either body. After confirmation, the classifier combines that view with the confirmed records. An unmatched headerless section can recover its sender and causal ids from a headerless in-flight record with the same trimmed text. That does not acknowledge the record: a system command remains `Sent` at turn start and waits for `Compaction`. The [transcript writer and spend consumer](./transcript.md#writing-entries) share the classified sections; neither widens the acknowledgement contract below.

A `TurnStarted` hook aligns the submitted prompt against the candidate `Prompt` batches for its card through [`align_submitted_prompt`](../../../crates/rimz/src/store/message.rs). Record text supplies the boundaries between adjacent messages: agent-, human-, and harness-authored records match through their structured headers (`SIGNAL` included), and system records match verbatim. Inside a batch the text and the blank-line joins must match exactly; only the outer whitespace follows the hook payload's normalization.

`Store::confirm_delivered_for_card_with` exposes the exact selected records to a callback inside the workspace lock, before the queue write. `DeliveryAckMatch` distinguishes `PromptCorrelated` from `OldestSentBatch`; a blank or absent prompt and compaction use the latter. The original `confirm_delivered_for_card` delegates with a no-op callback, preserving delivery behavior. Callback consumers must not reacquire the workspace lock or propagate enrichment failures into acknowledgment.

Unless the hook's run id names a subagent run, `harness::run::record_run_delivery` enrolls peers (a foreground `-p` peer carries its own non-subagent run id): it accepts only a prompt-correlated selection, a launcher-stamped root peer, and an adapter with native turn-start and turn-end hooks. It compares each agent sender's kind and non-absent `agent_id` to `launched_by`, never its name. Matching records create one running peer turn with the launcher's text and message ids, or append ids to the already-open turn without replacing its task. With a subagent hook run id, it instead [stamps the subagent answer](./subagents.md#the-lifecycle-end-to-end). The run codec writes inside the acknowledgment's lock; launch-prompt creation takes that same lock independently. An old mid-turn steer later confirmed by the no-prompt fallback is delivered but cannot enroll a human turn. A real launcher message whose turn-start hook omits prompt text likewise opens no peer run.

| Record state and event | Result |
| --- | --- |
| `Sent` + submission containing the batch exactly | `Delivered` |
| `Sent` headered batch + submission with surrounding text | `Delivered` with a mixed-submit reason and the stray byte counts; the surrounding text becomes direct-input transcript entries |
| `Sent` system batch + submission with surrounding text | No transition; headerless text requires an exact whole-prompt match |
| `Sent` + submission without the batch | No transition; the submission is direct pane input |
| `Queued` after an unconfirmed write + correlated submission | `Delivered`, with no time limit |
| `Claimed` + any submission | Ignored; the in-flight write owns the pane |
| `Sent` prompt + delivery window elapsed | `Queued`, keeping `last_sent_at` and incrementing `unconfirmed_sends` |
| `Sent` prompt + unconfirmed-send cap reached | `TimedOut` |
| `Sent` command + delivery window elapsed | `TimedOut`; commands are never resent |

Stray composer text around a headered batch is never a reason to write the pane again. A headerless system batch needs an exact match because its text alone cannot tell a delivery from the user's own words.

When a turn-start adapter reports no usable prompt text, confirmation selects the oldest `Sent` prompt batch with at least one record still inside its hold deadline, including any compaction deferral. If every candidate batch has expired, it falls back to the oldest, preserving late acknowledgements. Selection confirms the whole batch, including its expired members. A `Compacting` hook still selects the oldest `Sent` batch of `Command` records regardless of age. Correlation never selects a `Claimed` record, which leaves one duplicate window between the claim and the write's acknowledgement: settling inside it could not retract the in-flight paste, so the active deliverer keeps ownership.

Reconciliation runs in `message sweep` and `gc` (`reconcile_stale_messages`) and handles expired claims alongside stale writes:

- **Expired claim** (no completed write within `CLAIM_TTL`). The reconciler clears the claim stamp, `pane_id`, and `batch_id`, keeps `attempts`, and requeues the record with a `message.queued` event and `claim expired; sender gone`. At `MAX_DELIVERY_ATTEMPTS` (five claims), the record becomes `Abandoned` instead, preventing an endless crash loop.
- **Unconfirmed prompt** (no hook within `RIMZ_MESSAGE_DELIVERY_WINDOW_MS`, 30 s by default). The reconciler clears `pane_id` and `batch_id`, keeps `last_sent_at`, increments `unconfirmed_sends`, records `delivery unconfirmed; re-queued`, and the record retries through the normal FIFO path. A requeued batch member forms a new batch next time. At the cap the record becomes `TimedOut`.
- **Unconfirmed command** (no hook within `RIMZ_MESSAGE_COMMAND_DELIVERY_WINDOW_MS`, 3 minutes by default). The record becomes `TimedOut` with `delivery unconfirmed; command not resent`. A command reaches the pane at most once, because a duplicate `/compact` can discard context and a missing acknowledgement does not prove the first submit failed.

Sender-notice timing is independent of this delivery window; the [sender-notice delay overrides](../../reference/cli/message.md#the-message-header) change its five- and twenty-minute thresholds.

The hold also expires on read at the same body-specific deadline, even when no sweep runs; readers neither resend nor mutate the record. While the receiver's compaction bracket is open, the reconciler holds a stale `Sent` record in place and pushes `retry_after` one body-specific window ahead. Readers honour that deferral as the hold deadline. A composer queues a paste made during compaction and submits it when compaction ends, so a resend there would become a second turn.

A record whose agent has simply not reached a qualifying boundary is not failing. It stays `Queued` and no counter moves.

## Writing to the pane

`mux::PaneWriter` holds one per-pane advisory lock, at `RuntimePaths::pane_write_lock`, across a whole write batch: any compact-first command, every typed segment and pacing delay, the prompt paste, the `Sent` transition, and the final Enter. `rimz answer` and `rimz pane send` take the same lock, from any workspace. Different pane ids are independent; identical Zellij pane ids in separate sessions share a lock, conservatively. Steer waits for the lock instead of preempting another writer. Acquisition times out after 30 seconds as a mux error and follows the ordinary send-error recovery. Because the lock covers the gap between `Sent` and Enter, dispatch does not treat `Sent` records as blocking a fresh boundary send.

`PaneWriter::open` requires the session its writes address, and every writer passes the resolved room's session, so a write reaches that room's Zellij session from outside any pane. `rimz pane send` first checks that the room holds the pane, because a raw pane id reaches it with no lookup; `rimz message`, `rimz answer`, and the budget parker take their target from a fresh listing of that session. tmux ignores the session, because its pane ids are server-global.

### Paste, then submit

Interrupt is paste first, key second, inside `send::send_batch_to_live_pane` under one `PaneWriter`. With the lock held it rereads `store.snapshot_cached()`: only a `Running` or forced `Waiting` row gets the key, because on a resting row the paste itself starts the turn the key would cancel. It then writes exactly as steer does (smart-compaction command, paste, `Sent` record, Enter). Claude and Codex both hold text typed into a busy turn in their prompt queue, and Escape stops the turn and submits that queue as a fresh turn. After `RIMZ_MESSAGE_INTERRUPT_DELAY_MS` (default 3000) it presses the adapter's key once, still under the lock. No stop proof is awaited: RimZ shows no rested state between the interrupted turn and the queued one, so success is the record reaching `Delivered` on the fresh turn's `TurnStarted`.

A clean turn parked on background work already projects to `Success` in `rested_status()`, so it skips the key as a resting turn. Interrupt does not change that agent-state projection.

Two accepted provider hazards remain: the budget parker may press Escape at the same instant, producing double Escape and Claude's rewind dialog; Codex issue #42717 allows a long-running Unified Exec shell to survive a reported interruption, so a stopped turn does not mean every descendant process exited.

A `Prompt` is wrapped in bracketed-paste markers (`ESC[200~` to `ESC[201~`) through `MuxBackend::paste_text`, then Enter is pressed as a separate `send_key`. The close marker is what separates text from submit. Agent composers treat text and a trailing `\r` that arrive in one PTY read as pasted content, with the `\r` a literal newline. The composer leaves paste mode on `ESC[201~`, so the following Enter is a keystroke even when every byte arrives in one read. Adding a delay does not fix a submit problem on this path.

Inside the paste, LF and CRLF line endings travel as CR. Composers normalize CR to a newline but drop a bare LF, and terminal emulators and tmux's `paste-buffer` send CR for pasted line endings too, so multi-line prompts land multi-line.

The backends carry a large paste differently:

- **tmux.** A tmux command frame (about 16 KiB) cannot carry a full message body as an argument. RimZ loads a uniquely named buffer holding the opening marker, the CR-normalized body, and the closing marker through stdin, then pastes it with `paste-buffer -d -r` in the same invocation, so markers and body take the same PTY route even in copy mode. On tmux 3.7 and newer, `-S` turns off control-byte sanitization. `-r` keeps the byte stream raw and `-d` deletes the buffer; a failed batch deletes it best-effort before returning the original error.
- **Zellij.** RimZ writes the same bracketed byte stream in ordered chunks of at most 8 KiB (`ZELLIJ_WRITE_CHUNK`). On the first failed body chunk it stops, writes the closing marker best-effort, and returns the original error. The per-pane lock spans every chunk and the cleanup, but cleanup cannot retract delivered fragments or guarantee closure while the backend is down.

Explicit markers work even when the application has not enabled bracketed-paste mode.

### Commands are typed

A `Command` body takes the raw type path, because a composer treats pasted text as literal content and a pasted `/compact` would land as a prompt. The raw path is byte-faithful with no close marker, so timing does the separating work that the marker does for prompts. Composers can reassemble fast typed characters into a synthetic paste: Codex buffers characters less than 8 ms apart and suppresses Enter for 120 ms after the burst. RimZ therefore waits the command-submit delay (1 s, `RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS`) between typed segments and before Enter.

A command with arguments is typed in two segments. When the adapter appends an instruction to its compact command, RimZ types the declared command and its trailing space first, then the arguments one submit delay later. A composer can classify one long chunk as pasted text (Claude does past 800 characters); typing the command separately keeps the slash literal so it dispatches, and the paste-classified remainder becomes its trailing text. A bare command, multi-word plugin commands included, is one segment.

`rimz pane send` also uses the raw type path, since a bare shell would print the paste markers literally.

The send path spaces messages, not a paste and its submit: it sleeps one message interval (1 s, `RIMZ_MESSAGE_INTERVAL_MS`) before each message after the first, so fan-out members and a compact-then-prompt pair reach the composer as separate events.

### The Sent-before-submit barrier

[`write_batch`](../../../crates/rimz/src/message/send.rs) records the batch as `Sent` after all its text lands and before it presses Enter. A submitted message is therefore always preceded by its durable record and audit event. A crash between text and submit leaves a `Sent` record whose text sits unsubmitted in the composer, which the reconciler handles. A record that a cancel, clear, or archive finalized during the write is not recreated as `Sent`: the write logs a warning naming the message ids and still presses Enter, since the paste cannot be retracted and text left in the composer would glue onto the next write. The reverse order would let an agent start a turn RimZ has no record of.

### The message header

Every attributed delivery carries a header before the record text:

```text
Type: AGENT_MESSAGE
From: @swift-otter (planner)
Content:
<message>
```

| `Type` | Sent for | `From` |
| --- | --- | --- |
| `AGENT_MESSAGE` | A send from an identified agent caller | Stable reply address, followed by the profile (else kind) in parentheses unless it repeats the handle base |
| `USER_MESSAGE` | A human's `rimz message` | `@user` |
| `SUBAGENT_REPORT` | The status-only fleet digest when every row is a `rimz subagents` run | `@rimz` |
| `AGENT_REPORT` | The status-only fleet digest containing any solo agent row, including `-p --bg` runs and launcher-opened interactive peer turns | `@rimz` |
| `TEAM_REPORT` | An agent-launched team's leader, reported when the board flips to `Done` or the cohort ends before `Done`, followed by `Memory: <absolute board path>` as seen by the launcher | `@rimz` |
| `SUBAGENT_PAUSED` | A launched child parked on a provider limit, sent to its parent once per park ([backstops](./subagents.md#backstops)) | `@rimz` |
| `SUBAGENT_STALLED` | A launched child silent past the stall window, sent to its parent once per run ([backstops](./subagents.md#backstops)) | `@rimz` |
| `MESSAGE_UNDELIVERED` | An agent-sent message ended without delivery, sent once to its live sender | `@rimz` |
| `MESSAGE_QUEUED` | An agent-sent message remains queued past its receiver-class delay, sent once to its live sender | `@rimz` |
| `DEADLINE` | A deadline-ladder rung steered to the supervised child | `@rimz` |
| `WAIT` | A timer, command, or clock wait delivery | `@rimz` |
| `CACHE_KEEPALIVE` | A neutral prompt-cache ping for a sleeping agent ([policy](./loops.md#prompt-cache-keepalive)) | `@rimz` |
| `SIGNAL` | Every delivery fired by a `Trigger::Signal` row | `@rimz` |
| `STAGE` | A direct prose-only stage-open delivery from a flip or a resume or registration re-wake | `@rimz` |
| The notice name, upper-cased | A harness notice this binary does not know (`HarnessNotice::Other`) | `@rimz` |

An unknown harness notice keeps its string and takes ordinary harness delivery ([store.md § What is in it](../store.md#what-is-in-it) explains why the string survives). System records and `--no-from` sends carry no header.

A handle gains `#channel` when the delivery crosses lanes. The recipient's lane comes from its registered channel, its live pane channel, or the addressed channel, so a just-launched teammate in the same lane does not gain a spurious suffix before pane capture lands.

`address::agent_sender_handle` uses `agents::petname::sender_handle`: role, else explicit or pet name, else kind. Unlike the [listing handle](./fleet.md#petnames-and-the-canonical-handle), it never depends on a profile's or kind's current uniqueness. A matching live peer supplies identity and channel; otherwise the stored sender fields do. Even a bare-resumed launched agent carries a minted name recovered through ancestry. The kind fallback is for senders with no role or name, such as a hook-registered agent or unknown parent.

The receiver's turn-start hook parses the header into transcript entries ([transcript.md § Writing entries](./transcript.md#writing-entries)). `store::message::parse_sender_handle`, shared by header parsing and submitted-prompt alignment, accepts only `From: @<handle>` or `From: @<handle> (<label>)`: the handle is non-empty and whitespace-free; the label is non-empty, whitespace-free and contains no parentheses; nothing may follow the closing parenthesis. The label is stripped before writing `entry.from`, leaving the bare `@handle[#channel]` reply address.

## Compaction commands

Four callers send a native compact command through a `Command` record: smart compaction ahead of a prompt, idle compaction from the sidebar producer, flip compaction at a team hand-off, and the operator's `rimz agents compact`. Routing the command through a record gives it the same claim, retry, audit, and at-most-once write as any message.

`message/synthetic.rs::attempt_now` refreshes the elder's wake stamp after every miss, after recording the refusal verdict or delivery error. A queued `Resume` record contributes no deadline: `queue_head` excludes its control lane, so auto-continue owns its re-drive and a miss on an otherwise idle queue leaves no stamp. Claimed and sent resume records still arm recovery. A retryable post-claim stop preserves the attempt's own reason; only a terminal stop writes the caller's fallback. Standalone compaction uses this shared path for operator and automatic callers alike. The sweep retries queued commands at most once per delivery window until they deliver or, for an automatic command, expire.

An automatic command (`body == Command && automated`) is perishable: while `Queued`, it is valid for ten minutes from `enqueued_at`. The window is derived, not stored; `RIMZ_MESSAGE_COMMAND_VALIDITY_MS` shortens it for tests. `MessageRecord::is_deliverable` excludes an expired record at every FIFO head and blocker decision, even when no sweep runs. The next `Store::reconcile_stale_messages`, called by the sweep or `rimz gc`, alone finalizes it as `Expired`, recording the reason in `last_error` and `message.expired`. `message show` reports expiry before every other blocker; steer and interrupt cannot bypass it. Prompts, harness notices, operator commands (`automated: false`), and non-`Queued` records do not expire. A lapsed claim returns to `Queued` and then ages by its original enqueue time; a human requeue creates a new record with a fresh window. Smart compaction's released prompt can deliver on its own once its compact command expires.

`agents::compact_command` is the one composer every automatic caller uses: it picks the brief for the agent's seat, `CompactSeat::Team` when the launch-stamped `AgentState.team` is set and `Solo` otherwise, and `HarnessConfig::compact_instruction` returns a set `compact_instruction` for either seat or RimZ's brief for that seat. `rimz agents compact` bypasses the composer only for an explicit instruction. `LaunchSpec::compact_command` renders the adapter's native command. When the adapter declares `CompactInstruction::Trailing`, it appends [`[harness] compact_instruction`](../../guide/configuration.md#smart-compaction), folded to one line because a newline would submit it, and the send path types it as the [second segment](#commands-are-typed). Other adapters receive the bare command.

Two guards stop a repeated compaction:

- **The context baseline**, consulted by smart and idle compaction. `compacted_context_tokens` records the reading a compaction fired on; every caller stamps it when the reading is known. While a carried-forward stale gauge still equals that baseline, no further compact command is sent for it; a new reading re-enables it. Without this a stuck gauge would compact on every message.
- **The unprompted marker**, consulted by all three callers. `AgentState::compaction_unprompted` reads a durable timestamp set when a command reaches `Sent` (even without a context reading) or when `CompactionEnded { auto: Some(false), failed: false }` lands. Automatic and unknown-trigger compaction closes do not set it. It survives rollup rotation, linked successor adoption, and re-registration, and clears only on `TurnStarted`; a later `Delivered` acknowledgement cannot restore a marker its prompt hook already cleared. Pane-native manual compaction sets it only when the adapter reports the manual trigger (Claude, Pi, and Qwen do; a native Codex or OpenCode compaction reports an unknown trigger). For adapters without native turn-start hooks (plugins that declare no `turn_start`), the reducer records the marker but the predicate ignores it, because pulled observations cannot durably clear it.

Delivery re-checks the marker after claiming a command and before writing its pane. When the marker is set and the agent is still compacting, the command returns to `Queued`; when the compaction already finished while the command was parked, the record settles `Errored` with the no-repeat reason.

### Smart compaction

An agent compacts on its own only at the context ceiling (Codex around 90%), so a prompt sent near it can be cut in half by a compaction that fires mid-turn. `--smart-compact` compacts first, so the prompt lands against a fresh window.

Thresholds parse as `70%` (a fraction of the window), `120000` (absolute occupied tokens), or `180k` and `1m` (suffixed counts). Dispatch fills an omitted threshold from the [`[harness] smart_compact`](../../guide/configuration.md#smart-compaction) default for every caller, scheduled loop waits included. An unknown fill never triggers: the text sends untouched.

A percent threshold reads the same fill gauge the sidebar card renders (`context_fill_pct`). A token threshold reads `occupied_context_tokens`, which prefers the folded statusline breakdown, then the per-call split (cache reads plus cache writes plus fresh input), then the carried `total_tokens` gauge.

The two delivery modes compact differently:

- **Boundary.** Send the compact command alone, release the claimed prompt batch to `Queued` without an attempt penalty, and let `CompactionEnded` start a fresh delivery against the new window. A parked record reads the fill at the boundary, not at enqueue.
- **Steer.** Type the compact command, then one message interval later paste the prompt. Reconciliation holds that `Sent` prompt while the compaction bracket delays its confirmation.

Each fired smart compaction appends an `AutoCompact` assist record. A failed compact-first write settles the command `Errored` and fails the prompt's delivery attempt through the ordinary retry path.

### Idle compaction

Idle compaction sends a compact command to a resting team member, with no prompt behind it. [`harness/idle_compact.rs`](../../../crates/rimz/src/harness/idle_compact.rs) owns the shared producer/helper decision: `resolve_mode` chooses role `idle-compact` over `[harness] idle_compact`, `fire_point` resolves the idle threshold, and `should_compact` checks eligibility. The seat must have `AgentState.team`, not be a provider subagent, have a nonempty session id, and be effectively `Idle` or `Success`, without pending waits, an input request, budget park, or compaction marker. Occupied context must reach `IDLE_COMPACT_MIN_TOKENS` (50,000) and differ from `last_compact_command_tokens`; the adapter must expose a compact command. `cohort_live` reads `blackboard.md` through `scratch::board_stage` at the seat's worktree, not the pipeline projection: only `Done` refuses, so a missing board or worktree path counts live. PR state and teammate activity play no part.

The default `On` uses `HarnessConfig::prompt_cache_ttl`, resolving a per-provider override or `off` before the argument-free `ContextCapability::prompt_cache_ttl` default. Neither model nor account participates. The [configuration guide](../../guide/configuration.md#idle-compaction) owns the provider defaults and override grammar. Unknown lifetimes skip; invalid configured kinds or durations no greater than `PROMPT_CACHE_MARGIN` are refused at config load. `After(duration)` bypasses the TTL lookup; `Off` skips. `refresh_heavy_lanes` loads effective teams once per pass when any agent row has a team, sharing the result with idle compaction and the pipeline lane; without a team seat it passes empty teams to both. `config::effective::teams`, also used by the helper, owns the fallback to machine teams when the project root is absent or loading fails. Absent Markdown role values stay `None`, so a machine opt-out reaches them.

`fire_point` returns an `IdleWindow`: `On` fires from TTL minus the one-minute margin until, but not including, the TTL; `After(duration)` has no upper bound. Both timers and the helper use `AgentState::last_request_at()` ([activity clocks](../agents/model.md#activity-clocks)). A member found past its cache lifetime is left alone rather than compacted cold. Missing anchors never qualify, including rows without a completed turn, even under `After(duration)`.

Request anchoring excludes final generation from the margin. `cache_timing_reaches_the_request_before_expiry` tests the consumer decision exactly at and one second before both the fire point and the TTL, assuming a 1-second producer tick, 10-second helper spawn, and 10-second keystroke-to-request delay. Those 21 seconds fit inside the margin; they are runtime premises, not measured latency guarantees. The heartbeat and cache timing are best-effort: a slow pass or early eviction costs a cache write, never correctness.

The elected producer requires a bound pane and spawns detached `rimz agents idle-compact`, keeping store writes out of the sidebar's import graph. The request carries `agents::compact_command` for the team seat, including the team brief where supported.

The helper re-resolves the workspace, session, and pane, applies the shared decision to `Ctx::published_snapshot` (which folds pending waits and the activity heartbeat), and validates the command against the adapter and its configured instruction. Through `message::compact` it queues one automated `System` command with the `Done` gate, pins the pane, stamps `compacted_context_tokens` from its context reading, attempts boundary delivery, and appends an `IdleCompact` assist record. The optional `idle_after_secs` records the resolved threshold; `rimz stats` shows it when present and still accepts older records without it. A closed boundary leaves the command queued through the ordinary retry path.

Three layers make it once per idle stretch:

1. The producer's cache-class `(kind, agent_id)` record suppresses the same `last_request_at()` and throttles helper respawns for ten minutes. This pacing is producer-only: the helper must not veto its own spawn record.
2. The live queue refuses a second outstanding command, and `last_compact_command_tokens` suppresses the same occupied reading.
3. For adapters with native turn-start hooks, the unprompted marker stops the helper until the next turn opens, whether from a queued prompt or text typed into the pane.

Adapters without durable turn starts keep only the context-baseline guard.

### Manual compaction

`rimz agents compact @handle [INSTRUCTION]` resolves one agent with a bound pane and a native compaction command, renders the configured brief or the positional override, and calls `message::compact`. An explicit instruction is refused when the adapter does not accept trailing text; without one, those adapters get the bare command. The verb refuses adapters without native turn-start hooks (plugins that declare no `turn_start`), since the strict no-repeat guard cannot work for them.

`message::compact::refuse_repeat` refuses when any of three states holds: `is_compacting(now)`, a non-terminal `Command` record in the card's live queue, or `compaction_unprompted`. `send_compact` then queues a pane-pinned `Done`-gated command and attempts boundary delivery at once; a closed gate records a retry, so a lifecycle checkpoint or the elder sweep delivers it later. When occupied context is known it also stamps `compacted_context_tokens`. The record carries the human or calling-agent sender with `automated: false`, so it leaves a durable message and audit event but no assist record.

## Reply waits

`--wait[=DURATION]` turns a send into a synchronous scatter-gather: it stamps `reply_wait` on each record, then polls until every leg settles.

Preparation refuses what a reply wait cannot follow. Every target must be a lifecycle-bound card with installed, trusted hooks, checked once per login key. Broadcasts and fan-out are accepted; create-on-miss, schedules, `--after`, `--when`, bare pane targets, and unsubmitted pastes are rejected before dispatch.

Dispatch captures one frame-aligned event-log base before enqueue and copies it into every leg, so each leg folds terminal message events from its own cursor. Each `Sent` leg anchors a skip-existing transcript cursor before dispatch, so it reads only assistant messages written after the prompt.

Each leg is a two-phase machine:

- **Delivery.** Waits for `Delivered`, stamped by the prompt's own `TurnStarted`. A steer into a running turn opens the reply phase from `Sent + Running` instead, because the live turn emits no second `TurnStarted`: the rest of that turn is the reply. Interrupt never takes that shortcut: `--interrupt --wait` follows only the fresh turn acknowledged by `Delivered`.
- **Reply.** Legs read status through `TurnWaitView`, which loads the loop catalog's pending waits, then the message queue, then the rollup. That is the reverse of the order a wake publishes them (message record, row consumed, turn start, delivery ack), so a wake in flight shows in at least one read. A rested agent with pending waits, or with an undelivered `WAIT` or `SIGNAL` harness message whose sent hold has not expired, reads as `Sleeping`. Legs settle through `TurnCompletion`: `Idle` or `Success` completes the leg only once a turn has opened (`turn_started_at` set). `Failed`, a delivery failure, a vanished card, or a skipped waiting input fails it while the other legs keep gathering. `Waiting`, `Paused`, and `Sleeping` stay inside the reply and keep its turn anchor. A `Running` card whose `turn_started_at` differs from the anchor ends the leg unless its current `AgentState::prompt` contains wake notices alone, as classified by `store::message::prompt_is_wake_only`: every section must be a `WAIT` or `SIGNAL`. A wake-only turn re-anchors the leg on its new stamp and keeps waiting, even when no poll saw `Sleeping`. A human or peer prompt, or a batch mixing a wake with another section, opens an unrelated turn and ends the wait.

`TurnWaitView::wake_in_flight(agent, digest)` owns the non-terminal harness-message test. The view captures one clock beside its queue read and excludes a `Sent` record past its hold deadline, including any compaction deferral, without reconciliation. Reply waits pass `false`, counting `Wait` and `Signal` only; the supervised-run owed predicate passes `true` to also count `SubagentReport`. A digest alone still does not hold a reply wait open.

One 500 ms poll reads the message list and cached snapshot once per tick and advances every unfinished leg. On entry and every tenth tick (`WAIT_GUARD_TICKS`) it also folds agent-context sidecars and re-checks for cycles.

### Cycle detection

Two agents that each wait on the other would hang forever, so [`wait_cycle`](../../../crates/rimz/src/message/reply.rs) builds a wait graph and refuses to enter a cycle.

An edge runs from a sender card to a receiver card in two cases:

- A live `reply_wait` record that is `Queued`, `Claimed`, or `Sent`, from a named `Running` sender.
- A `Delivered` `reply_wait` record from history, when the receiver is `Running`, its `turn_opened_by` context lists the record, and the sender is named and `Running`.

Dispatch checks the graph before enqueue, and the periodic poll catches simultaneous dispatches and cycles that form later. The youngest `msg_` id in a detected cycle yields: only that reply leg fails, and its record is left untouched so the turn boundary still delivers the text. Older waits continue. Diagnostics name the blocking handle and message id and render the multi-hop chain.

### Settlement

The join succeeds only when every leg completes; otherwise it takes the first non-completed status in target order. `--any` returns the first terminal leg without canceling the rest.

One deadline spans fan-out dispatch and every reply turn. A human's bare `--wait` is indefinite, an agent's defaults to one hour, and an explicit duration wins. On expiry every unfinished `Sent` record is marked timed out, every unfinished leg is classified `TimedOut`, and the command exits 124.

Before polling, the CLI prints each ordinary delivery receipt on stderr, including in JSON mode. Text output streams labeled replies in completion order; `--json` buffers one handle-keyed map. A single agent that writes no assistant message yields empty stdout and a note on stderr. Each `ReplyResult` carries its leg's last observed message status, so deadline hints describe the same queued, sent, or delivered state that timeout settlement acted on without re-reading the store. The timeout header and per-leg hints go to stderr in both output modes.

## Scheduling and wakeups

The room's elected sidebar elder notices when a parked message comes due, through a deliberately thin handoff:

1. The CLI writes `message-wake.json` under the runtime root with the earliest time worth a look: an ordinary-lane `not_before`, `Queued` retry floor or ready-queued backstop, a `Claimed` lease expiry, or an unconfirmed `Sent` reconcile deadline (30 s for prompts, 3 minutes for commands, per [Confirmation and retry](#confirmation-and-retry)). Queued `Resume` records arm nothing because the sweep excludes their control lane; auto-continue re-drives them. A standalone compaction left `Queued` arms this stamp too, whether operator-requested or automatic, so its sweep does not depend on later message traffic.
2. The elder reads only that file, and when the stamp comes due spawns a detached `rimz --mux <mux> message sweep --workspace-id <id>` ([`fire.rs`](../../../crates/rimz/src/message/fire.rs)), passing its room's workspace id and mux by argv. The elder does no store reads, store writes, or message logic.
3. The sweep finalizes expired queued automatic commands, reconciles stale `Sent` records and expired `Claimed` records, evaluates unmet conditions, delivers ready FIFO heads, then rewrites or removes the wake stamp.

The sweep is single-flight through a `message-sweep.lock` file lock, so overlapping wakeups collapse into one pass.

An ordinary-lane queued perishable command's wake deadline is capped at `expires_at`, even when its schedule or retry floor is later. That wake tightens cleanup latency; read-time expiry releases its FIFO slot without any helper.

Condition evaluation inside a sweep is one transaction. It evaluates every unmet condition against one context-enriched snapshot, applies every stamp, retry floor, and watched-agent archive together, reloads the pending records, and delivers newly eligible heads from the same snapshot in the same run unless their card was held in the queue read after reconciliation and before that snapshot. A new stamp emits `message.after_met` or `message.when_met`.

The sweep backs off because the elder ticks often. When it cannot deliver a ready head (gate closed, ask waiting, compacting, no pane, behind a `Sent` prompt), it sets `retry_after` one delivery window ahead, so the elder retries at most once per window. On a `Queued` record, `retry_after` is only a wake hint: it does not affect `is_ready`, FIFO position, claim leases, or hook-driven delivery. Its one exception is a compaction deferral on a `Sent` record, which extends the read-side hold, including FIFO and hook-driven boundary delivery.

A `NoPane` back-off also records the blocker in `last_error`, in the same queue commit and in the words `rimz message show` prints. An ended receiver is terminal instead: the sweep archives open records for the card, plus records with unmet `when` conditions watching it, only when their `enqueued_at` is at or before the observed `ended_at`. The writers apply this cutoff under the workspace lock so a concurrent resume's newer messages survive a stale sweep snapshot; the end-hook reactor keeps its unbounded archive. The receiver reason is `receiver ended; rimz message @<handle> resumes it` for a launched child, or `receiver ended` otherwise. Other refused heads, including a starting provider, move `retry_after` alone. The deferred record stays `Queued` with its pane pin and its `attempts` untouched, and the next claim clears the error.

The sentence does not outlive the blocker. A defer on any other verdict clears a `last_error` that a `NoPane` back-off wrote, so a pane that returns while the receiver is busy leaves the gate as the only thing `rimz message show` reports. It clears its own sentence and nothing else: a real send failure recorded by a requeue stays, which is why the recognizer sits beside the formatter in `message::deliver` and the store applies it without reading the words.

A ready `Queued` head arms the stamp even without `not_before`, contributing its `updated_at`. That backstop recovers a message to an idle agent that missed the live send.

An unmet `when` condition sets `retry_after` to the exact projected trip time, so a 58-minute dwell wakes once at 58 minutes. When the watched session ends, every record still waiting on it is archived with the condition in `last_error`; the lifecycle hook does this in realtime and orphan GC is the backstop. A met stamp survives session end and receiver delay, which is how a busy receiver still gets the message at its next boundary.

Durations accept `s`, `m`, `h`, and `d`; zero is rejected. Wall-clock `HH:MM` resolves to its next occurrence in the configured `timezone` (today if still ahead, else tomorrow), falling back to the system zone.

`rimz message --schedule` and `rimz wait` feel alike and work differently. A schedule is a floor on a record that already exists: the text is written now and held until its time. A wait is a loop task that holds no message until its trigger fires, then dispatches one as an automated `Harness { notice: Wait }` send with fan-out disabled and no caller attribution. That is why a wait can wait on a signal or a command, and why its text appears in the queue only when it lands ([loops.md § Waits](./loops.md#waits)).

### Sender notices

`crates/rimz/src/store/writer/queue.rs::QueueTxn::close` collects agent-sent, non-`reply_wait` endings in `Archived`, `TimedOut`, `Abandoned`, or `Expired`. `Errored` is left out: a send failure errors inside the sender's own `rimz message` call, which exits non-zero with the reason, so a notice would repeat it. A queued compaction command refused as a repeat also ends `Errored` without a notice. `crates/rimz/src/store/writer/queue.rs::Store::commit_queue` resolves live senders and queues a `MESSAGE_UNDELIVERED` notice and its `message.queued` event in the same commit as the ending. Delivered and deliberately canceled messages do not qualify. A closed record leaves the live queue once, so an ending cannot notify twice; a notice is `Harness`, never `Agent`, so its own ending cannot recurse. The notice quotes the ending reason and attempt evidence, previews the text, and asks the sender to inspect the receiver before deciding whether to resend. The archived receiver's existing resume hint is unchanged.

`crates/rimz/src/store/message/sender_notice.rs::resolve_sender` chooses an unended card of the sender's kind: when the sender has `agent_id`, it must match the card's session id or launch id; otherwise a recorded name must match, and when several live cards share that name, as team roles do across lanes, the one in the sender's recorded channel. It never falls back from a stale id to a recycled name. With no live sender there is no notice, only the ending's original audit. The notices have the sender card's channel, a `Done` gate, no pane pin, and no `in_reply_to` causality. Both are ordinary queued prompts and neither counts as an owed wake. `reply_wait` records get neither notice because their blocked CLI already reports failures and timeout hints.

The sweep classifies a refused receiver through an exhaustive `crates/rimz/src/message/deliver.rs::DeliveryVerdict::stall_class` match:

| Class | Delay | Verdicts |
| --- | --- | --- |
| NotWorking | 5 min | `NoPane`, `ProviderStarting`, `ResumeUnrecovered`, `AskWaiting`, `ReceiverGone`, `ReceiverEnded` when not archived, and `GateClosed` with a status other than `Running` (including absent status) |
| Busy | 20 min | `GateClosed` with `Running`, and `Compacting` |
| No notice | None | `Scheduled`, `WaitingOnAfter`, `WaitingOnWhen`, `BehindFifo`, `Expired`, `Ready`, or a stop without a verdict |

The [millisecond overrides](../../reference/cli/message.md#the-message-header) use the same `env_ms` convention as the delivery window. Each qualifying pending record on the refused head's card is measured from its own `enqueued_at`, not just the head. Its own schedule and conditions must be met. An unconfirmed-send requeue keeps that clock, while `message requeue` creates a fresh message. `crates/rimz/src/store/writer/queue.rs::Store::queue_still_queued_notice` rechecks `Queued` and absent `queued_notice_at` under the workspace lock, then stamps that field and queues `MESSAGE_QUEUED` with its audit event together. No second call changes either surface. The original message stays queued and the notice names `rimz message cancel <id>` as the withdrawal path.

The sweep's final wake refresh covers notices it queues. Out-of-sweep endings refresh through the lifecycle end reactor, GC after archival and reconciliation, and worktree creation or retirement. The worktree library has no `ResolvedWorkspace`, so its direct and automatic-retirement CLI callers refresh after the operation, including agent recreation and both GC/worktree sweeps. No ending-notice timer is added. Queue truth and the notice record remain authoritative when a wake refresh fails. Notices have no assist record; their ordinary message record and queued audit event are the durable account.

`queued_notice_at` defaults to absent on legacy records, survives queue rewrites and terminal history, and is never cleared on the same message. An older binary rewriting a mixed-version room drops the unknown field, so a later newer sweep can produce a second queued notice. This accepted compatibility limit adds neither a status nor an event method; older readers preserve the new harness-notice strings through `HarnessNotice::Other`.

## Storage and audit

```text
records/messages/messages.jsonl   live Queued, Claimed, and Sent records
audit/messages/<bucket-start>.jsonl    terminal records, with text
log/events.log.jsonl          message.* audit events, without text
```

The queue file holds only live records and is the truth. One queue transaction rewrites `messages.jsonl` when the live set changed, then appends terminal records to history, then appends the audit events. History uses the shared epoch-aligned seven-day bucket rule from `disk/buckets.rs`, keyed by each record's `updated_at`. `list_history()` reads buckets newest first and stops at the newest 500 records (`HISTORY_READ_LIMIT` in `store/message/codec.rs`), the read bound the old write-time trim gave every caller. The audit class sweep owns age and size retention. Every write holds the workspace lock; the queue rewrite uses temp-file-plus-rename and history uses the append helper ([store.md § Write classes](../store.md#write-classes)). The queue file is created lazily, so an empty workspace costs the hook path one missing-file stat, and a missing file reads as no live records.

History is audit, so an append failure warns and leaves the queue transition standing. This order matters with mixed binaries: if a history file an older binary cannot parse could fail the transaction, the queue would keep a record its caller believes settled and the next sweep would deliver it again. A crash between the queue rewrite and the appends costs the terminal text and its audit event, never queue state.

`Expired` adds a status an older binary cannot decode. Once an expired record is in `audit/messages/`, older binaries sharing the workspace store fail history reads used by `message list`, `message show`, `message requeue`, and `send_compact`'s settled lookup until upgraded. The terminal commit removes it from the live queue, so older queue transactions never encounter that status. Event-log readers retain unknown methods through their existing catch-all.

The store reads through `list()` (live), `list_history()` (terminal, with text), and `list_pending()` (`Queued` only).

The event log carries these methods: `message.queued`, `message.edited`, `message.after_met`, `message.when_met`, `message.sent`, `message.delivered`, `message.timed_out`, `message.errored`, `message.canceled`, `message.abandoned`, `message.expired`, and `message.archived`. The parser reads the retired `message.removed` as a cancellation. Not every transition writes one: a claim, a retryable pre-send failure returning a claim to `Queued`, a `Sent` record held during compaction, and a sweep that only moves `retry_after` and its blocker rewrite the queue silently. An explicit claim release (`release_message_claims`) does write `message.queued`.

The payload carries `message_id`, `address`, `kind`, `agent_id`, `agent_name`, `channel`, `gate`, `status`, `body`, `pane_id`, the `forced` flag, sender attribution, `text_len`, both counters, timestamps, the compaction baseline, and an optional `reason`. The reason is set on errors, abandons, cancels, archives, expiries, releases and requeues, timeouts, edits (the changed fields), condition stamps, and mixed-submit deliveries. **Message text never enters the event log**, which is why `message list` merges three sources: live records, history records with text, and terminal rows known only from events.

## The inbox verbs

Flags and rendering are in [cli/message.md](../../reference/cli/message.md). Underneath:

- `message list` and `message show` merge the three sources above. `list` shows only conversation records unless `--system` is passed, independently of `--all`, lane, status, or target; JSON follows the same filter. Human output reports a hidden-system count when it is nonzero, scoped before the row limit, even when no visible rows remain; JSON omits the count. `show` reads every sender class and renders the [ordered check](#the-ordered-check), naming the first unmet condition. The rendered handle comes from the record's enqueue-time `address`, then the live snapshot, then `agent_name` plus channel, then `kind:agent_id`.
- `message edit` is the single compare-and-swap path for a queued record. It accepts only `Queued`, refuses `Claimed` as in flight, reports terminal records from history, applies the delivery changes, clears `retry_after` so the next sweep sees them, and appends `message.edited` naming the changed fields. Receiver, channel, card, sender, and pane affinity are not editable: retargeting is cancel plus send.
- `message steer` pushes one queued record through now ([The delivery helper](#the-delivery-helper)).
- `message interrupt <id> [--force]` pushes one queued record through the paste-then-key path. Claimed and terminal records are refused as for steer.
- `message requeue` copies a terminal history record into a fresh `Queued` record with a new id, keeping text, receiver, channel, sender, body, delivery settings, and `in_reply_to`, and clearing condition stamps. A terminal row known only from events cannot be requeued, because its text was never stored there.
- `message cancel` settles named live records. `message clear <target>` settles every open record for one card, and a targetless `message clear` settles the scoped lane. Both include system records hidden from the inbox, and `clear` prints the ids it canceled.

Two hidden helpers do the pipeline's background work, each spawned detached: `message deliver --message-id <id>` and `message sweep --workspace-id <id>`. The sweep opens the recorded room by id without recording it again; the id wins over `--root`, and a missing workspace record is an error. Without an id, it resolves the participant room and opens only an existing store: no room means ``no room at <project_root>: run `rimz start` there, or pass --root <room>; nothing to sweep``, a nonzero exit, and no state created.

## Channels

A channel is the cooperation lane inside one room. It is the group the sidebar draws, the `#channel` suffix an address takes, and the tab name RimZ recovers on rebirth.

### Where a lane comes from

Launch resolves one lane and stamps it into `RIMZ_CHANNEL`, the launch event, and the rollup. [`resolve_room_channel`](../../../crates/rimz/src/harness/spec.rs) takes the first that applies:

1. An explicit `--channel`.
2. The current directory's basename, whenever it differs from the project root. A RimZ-owned worktree is the common case.
3. `<dir>/<team>` for a named team launched at the room root.
4. No lane, for a bare directory room.

Read paths use the stamped lane and fall back to the worktree basename only for agents with no stamp. Hooks resolve the git toplevel from the agent's own cwd at any depth, so an agent in a nested checkout gets that checkout's lane, and a non-git agent at the room root folds into the room's root lane.

Lane equality scopes target resolution, rendered handles, sidebar grouping, `agents list`, pane overlays, `message list`, transcripts, and recovery. Branch names are display metadata on the worktree card and never define a lane.

### The shared namespace

No lane has a record of its own. Worktree lanes take their durable truth from the `rimz-worktree.json` marker ([worktrees.md](./worktrees.md)), and explicit, team, and directory lanes derive from the launch identity stamped on each agent. Worktrees still list without agents, but an empty explicit lane has no independent record to list or restore. A rebirth restores agent tabs only.

The sidebar is presence-driven, so a group appears only while a pane runs in that lane.

Explicit lanes and RimZ-owned worktrees share one namespace, and `channel.rs` owns both halves of the rule. An explicit `--channel NAME` launch is refused for a malformed name and for a name a managed worktree owns (`channel::admit_launch`, a Git read with no store access). `rimz worktree new NAME` and a launch that would create an explicitly named worktree `NAME` are refused while a live agent holds the lane (`channel::admit_worktree_name`): an agent row with no end stamp, liveness other than dead, the lane stamped, and a checkout that is not the worktree of that name. Launch admission checks the dashed name before checkout creation; an existing worktree is entered without this check, including fresh replacement. Resume, generated names, and default `pr-<N>` names are unchanged.

`records/channels.json` is retired. Earlier builds kept explicit lanes there; no code reads or writes it, and `rimz gc` removes a leftover file under the `records` class.

### Addressing into a lane

Commands run inside a stamped pane inherit `RIMZ_CHANNEL`, so `@claude` resolves within that lane. Without that stamp, an agent uses its stored card channel, even from the main checkout. Bare `message clear` therefore clears that channel's messages. A human shell in a bare directory room has no lane and reaches the whole room; `message list` treats that as the main-lane inbox, and `--all` widens it. See [channel resolution](fleet.md#the-address) for the shared precedence.

`--worktree` and `--channel` are separate launch intents. A worktree launch creates or reuses a Git checkout; a named-channel launch stays in the room root and records only the lane. Inline `#design` and `--channel design` reconcile through the same target parser, so a mismatch fails before delivery.

## Hazards

- **Queued text can land on a half-typed human draft.** Delivery gates on store state, never on focused-pane state or captured composer contents, because a pane read is latency, not truth.
- **Agent UIs can show dialogs no hook reports.** Delivery never captures the pane. A script that must inspect UI text captures before sending through the public `pane capture` primitive.
- **Multiplexer writes are best-effort.** A pane can vanish or reject input after the claim; the record stores the error and retries to the cap.
- **A pane write is not an acknowledgement.** Treat `Sent` as pending. Anything that assumes the agent read the text belongs after `Delivered`.

## See also

- [transcript.md](./transcript.md): the conversation log confirmed deliveries write, and asks and answers.
- [fleet.md § The address](./fleet.md#the-address): the address grammar dispatch resolves through.
- [loops.md § Waits](./loops.md#waits): the loop tasks that dispatch messages when a trigger fires.
- [subagents.md](./subagents.md): the digest producer behind the join guard.
