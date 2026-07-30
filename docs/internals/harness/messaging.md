# The message system

> How text reaches a running agent: the durable record, the delivery decision, the pane write, the reply wait, the channel lanes that scope addressing, and the transcript that reads the conversation back. The code is `crates/rimz/src/message/`; [fleet.md](./fleet.md) is the map for this area and owns the [address grammar](./fleet.md#the-address) this module resolves through. For users, the commands are [cli/message.md](../../reference/cli/message.md), [cli/transcript.md](../../reference/cli/transcript.md), [cli/channel.md](../../reference/cli/channel.md), and [cli/asks.md](../../reference/cli/asks.md).

## What the module does

Agents run their stock CLIs in real terminal panes. RimZ has no API into them, so the only way to give an agent work is to type into its pane, exactly as a human would. `rimz message` does that on behalf of a human, a script, a loop task, or another agent.

The hard part is not the typing. It is that the receiver is usually busy: mid-turn, blocked on a permission prompt, compacting its context, or not yet launched. Typing into a pane at the wrong moment either interrupts work in flight or lands in a composer nobody submits.

One rule resolves that:

> **The durable record is the message; the pane write is only an attempt.**

Every send persists a `MessageRecord` before a single byte reaches a pane. If the receiver can take the text right now, the same call writes it and marks the record `Sent`. If it cannot, the record stays `Queued` and the agent's next turn boundary delivers it, oldest first. A busy agent, a closed and reopened room, a multiplexer write that fails, a crash between claim and send: none of them lose the text, because the text was never only in flight.

Everything else in the module is a consequence: how a record decides it is ready, who wakes up to deliver it, how a write is confirmed, and what happens when confirmation never arrives.

## Module layout

Start here when you are looking for where a behaviour lives.

| File | Owns |
| --- | --- |
| [`message.rs`](../../../crates/rimz/src/message.rs) | The vocabulary: `MessageRecord`, `MessageStatus`, `DeliveryGate`, `AfterCondition`/`WhenCondition`, `AutoCompact`, card matching, FIFO head and batch selection, and the environment knobs. Pure logic, no I/O. |
| [`message/dispatch.rs`](../../../crates/rimz/src/message/dispatch.rs) | One send request end to end: resolve targets, bind conditions, preflight hooks, build records, decide park-vs-live, order the fan-out. |
| [`message/send.rs`](../../../crates/rimz/src/message/send.rs) | The pane write: payload construction, the bracketed paste, the submit barrier, pacing, and the compact-first command. |
| [`message/deliver.rs`](../../../crates/rimz/src/message/deliver.rs) | Readiness: the ordered delivery check, the delivery attempt and its failure recovery, condition evaluation, the sweep, and the wake stamp. |
| [`message/reply.rs`](../../../crates/rimz/src/message/reply.rs) | `--wait`: leg state machines, transcript anchoring, cycle detection, join settlement. |
| [`message/fire.rs`](../../../crates/rimz/src/message/fire.rs) | The elder's side of the clock: read the wake stamp, spawn `message sweep`. Nothing else. |
| [`store/writer/queue.rs`](../../../crates/rimz/src/store/writer/queue.rs) | Every status transition, under the workspace lock, with its audit event. |
| [`store/message_store.rs`](../../../crates/rimz/src/store/message_store.rs) | The JSONL queue and history files. |
| [`cli/message/`](../../../crates/rimz/src/cli/message) | Flag parsing, rendering, and the inbox verbs. No delivery logic. |
| [`cli/hooks/lifecycle/delivery.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/delivery.rs) | The hook side: confirm sent records, spawn the delivery helper at turn boundaries. |

Three neighbours carry pieces this module leans on: [`harness/target.rs`](../../../crates/rimz/src/harness/target.rs) resolves `@handle#channel` addresses and renders handles, [`transcript.rs`](../../../crates/rimz/src/transcript.rs) is the durable conversation log, and [`channel.rs`](../../../crates/rimz/src/channel.rs) is the named-channel registry.

The layering is one-directional: `dispatch` calls `deliver` and `send`, `deliver` calls `send`, and `send` calls the store and the mux. Nothing calls back up.

## The record

A record is keyed on a **card**, the logical agent identity the rollup tracks: a kind plus a session id, with the stable `agent_name` as a second key ([model.md § The rollup](../agents/model.md#the-rollup)). The name matters because an agent can be addressed before it registers a session. A message queued against a provisional `launch_*` id keeps that id, and `same_card` folds it into the real session's queue once registration lands. One card, one FIFO queue.

`msg_` ids are workspace-unique and time-sortable, so string order is FIFO order. The module relies on this everywhere; do not replace the id scheme without replacing the ordering.

| Field | Purpose |
| --- | --- |
| `message_id` | `msg_` prefixed, time-sortable, FIFO by string order |
| `workspace_id` | owning workspace |
| `kind`, `agent_id`, `agent_name` | receiver card; the name folds provisional ids into the registered queue |
| `address` | receiver handle as resolved at enqueue, rendered by `list` and `show` after the live card is gone |
| `channel` | receiver lane at enqueue time |
| `sender` | `Human`, `Agent { kind, name, profile, role, channel }`, or `System` (renders as `rimz`) |
| `automated` | background orchestration traffic; never earns a dollar-budget waiver |
| `reply_wait` | a CLI is blocked on this record's reply ([Reply waits](#reply-waits)) |
| `in_reply_to` | the messages that opened the sender's authoring turn; empty starts a new conversation root |
| `body` | `Prompt` (pasted) or `Command` (typed, such as `/compact`) |
| `text` | the content |
| `enter` | submit with Enter after the write |
| `gate` | `Done`, `Any`, or internal `Resume`: which agent statuses release delivery |
| `force` | deliver past an agent that is holding a question |
| `pane_id` | pane affinity when known at enqueue; cleared on retry |
| `status` | lifecycle state, below |
| `not_before` | scheduled delivery floor |
| `after`, `when` | cross-agent conditions, each with a durable `met_at` stamp |
| `auto_compact` | context-fill threshold that fires a `/compact` ahead of the text |
| `compacted_context_tokens` | the reading a compaction fired on, so a stale gauge cannot fire it twice |
| `batch_id` | shared by records written in one paste; an uncorrelated turn start confirms the batch together |
| `retry_after` | wake-only backoff hint set by the sweep; it never gates FIFO or delivery |
| `attempts`, `last_attempt_at` | pre-send claim bookkeeping; caps at `Abandoned` |
| `last_sent_at` | last pane write; survives a prompt requeue so a correlated late acknowledgement can settle it |
| `unconfirmed_sends` | prompt writes that reached a pane but were never confirmed; caps at `TimedOut` |
| `last_error` | the most recent delivery or reconciliation failure |
| `enqueued_at`, `updated_at`, `delivered_at` | timestamps |

Two counters, two caps, two meanings. `attempts` counts claims that failed before any byte was written and caps at `MAX_DELIVERY_ATTEMPTS` (5). For prompts, `unconfirmed_sends` counts writes that landed and were never acknowledged and caps at `RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS` (3). A claim bumps only the first; a stale-`Sent` prompt requeue bumps only the second. Commands do not use the unconfirmed-send cap because they are never resent after reaching a pane.

## Status lifecycle

```text
Queued ──► Claimed ──► Sent ──► Delivered
   │          │          │
   │          │          └──► TimedOut   (unconfirmed delivery)
   │          ├──► (back to Queued on pre-send failure)
   │          └──► Abandoned   (pre-send retry cap)
   │
   ├──► Canceled   (user)
   ├──► Archived   (receiver ended, or channel torn down)
   └──► Errored    (address resolved to nothing)
```

`Queued` and `Claimed` are open: the record is live in the queue and `is_open()` is true. `Claimed` is a short lease taken immediately before a write so a concurrent deliverer cannot double-send; a claim older than `CLAIM_TTL` (15 s) is treated as expired, so a crash mid-send leaves a redeliverable record rather than a stuck one.

`Sent` means bytes reached the pane. It is not terminal, because a write is not proof the agent took the text: the record stays live until a lifecycle hook confirms it or the reconciler gives up.

`Delivered` means the agent acknowledged: `TurnStarted` for a `Prompt`, `Compacting` for a `Command`. One body cannot confirm the other.

The six terminal states are final. A terminal transition appends the full record to `messages/history.jsonl`, removes it from `messages/messages.jsonl`, and appends a `message.*` audit event carrying no text.

| Trigger | Terminal status |
| --- | --- |
| A lifecycle hook correlates with the submitted record | `Delivered` |
| An unconfirmed command reaches its delivery deadline | `TimedOut` |
| An unconfirmed prompt hits the resend cap | `TimedOut` |
| The address resolved to no agent, after the durable fallback | `Errored` |
| `message cancel` or `message clear` | `Canceled` |
| Pre-send retries exhausted | `Abandoned` |
| Receiver session ended, a watched `when` session ended, or the channel was torn down | `Archived` |

`Archived` is bookkeeping, not failure: it means the conversation the record belonged to no longer exists. `message list` hides archived records; `--all` and `message show` keep the audit trail readable. Lifecycle `Ended` archives in realtime and `rimz gc` is the durable backstop.

## Sending

### Three modes on one timing axis

All three resolve targets through the same parser, write the same record shape, ride the same pane primitive, and emit the same audit events. They differ only in *when* the record is allowed to deliver.

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Steer | `--steer` | Write to the live pane now, interrupting the turn. Conflicts with `--schedule` and `--on`, which are gates it has no use for. |
| Boundary | default | Write now if the receiver can take it, otherwise park for the next qualifying turn boundary. |
| Schedule | `--schedule <DUR\|HH:MM>` | Always park, with a `not_before` floor. |

Steer still writes a `Queued` record first and moves it to `Sent` when the paste lands. When the address resolves only to a durable card with no live pane, steer parks instead of dropping, prints `queued for @handle (msg_...)`, and the retry path delivers when a pane appears.

Two flags add cross-agent gates to the boundary mode, and they compose with each other and with `--schedule`:

- `--after <ADDR>` holds until that agent finishes its queued work. Each address must resolve to exactly one durable card; repeats form an all-of set. Naming the recipient itself is rejected, and so is a fan-out address.
- `--when '@handle <status> <duration>'` holds until one agent stays continuously in a **raw** lifecycle status for the dwell. Self-reference is valid here, which is what makes keep-warm messages (`@codex --when '@codex idle 58m'`) work.

The raw-versus-effective distinction is load-bearing. Delivery gates read `effective_status()`, which projects budget parks to `Paused`, settles hookless turns, and reads a clean turn parked on background work as `Success` while the background chore runs. `when` conditions read the stored `status` field directly, so a projection never trips a dwell the event log cannot justify.

### The dispatch walk

[`dispatch()`](../../../crates/rimz/src/message/dispatch.rs) is the one entry point for an owned send. Its sequence:

1. **Load what the decision needs.** Pending records (boundary mode only), and agent-context sidecars when a smart-compact threshold applies, conditions, an agent sender, or `--wait`.
2. **Choose a snapshot.** Producing a full resolution snapshot means talking to the multiplexer. When every target already parks (no live pane needed, no lazy-registering kind, no provisional id), `targets_all_park_without_live` proves the cached rollup is enough and dispatch skips the mux round trip entirely. This `rollup_only` path is a latency optimization; correctness never depends on it.
3. **Resolve targets.** Live agents and live panes both, combined so one agent with a bound pane yields one target rather than two. When the live view finds nothing, the durable audit rollup is the fallback, with pane-shadowed co-resident sessions filtered out first. For intrinsic `@all`, an explicit launch caller captured by the CLI is resolved against that durable rollup and removed before arity checks, condition binding, reply preparation, or delivery; no peers is a dispatch error. Human callers and explicit selector fan-outs are unchanged. A multi-match without `--all` or `@all` is an ambiguity error listing the candidates.
4. **Bind conditions.** Each `after` and `when` address resolves once and pins a card. A condition that is already satisfied gets its `met_at` stamped immediately, which is why upstream work must be queued *before* the message that waits on it.
5. **Preflight hooks.** Any target that will park requires installed, trusted hooks for its kind, checked once per kind. Turn-end hooks are the delivery trigger, so parking without them would queue text nothing will ever release.
6. **Decide park or live, per target.** A schedule or unmet condition parks first. Receiver readiness then comes from the exact pane binding when available or the durable card on the rollup-only path; a rejected write carries either the effective status or the native-input wait through `DispatchOutcome::Queued`. After that readiness decision, an unresolved pane or ready queued backlog parks without adding a reason. Jumping an existing queue would reorder the conversation. Boundary receipts say `delivered to @handle (msg_...)` after a live write, add the status and `rimz message steer msg_...` to a reasoned park (plus `--force` for native input), and keep reason-free parks at `queued for @handle (msg_...)`. Because the rollup-only decision deliberately skips the multiplexer, a status-aware receipt does not prove a pane is still live; `message show` performs the full check.
7. **Enqueue and attempt.** Every target gets its durable record. Live targets go straight into a delivery attempt; parked ones stop at `Queued`.
8. **Rearm the wake stamp** so the elder knows when to look again.

Fan-out follows the same path per target, paced one message interval apart, prefixing each delivery with the addressed handle (`@all,`) so receivers read it as a group message. The prefix remains even when caller exclusion leaves one peer. A blocked member is skipped while the rest send, and the summary names sent and skipped agents with their message ids.

A genuine no-match, after the durable fallback, writes a terminal `message.errored` bounce carrying the raw address, so `message list --all` shows the failed hand-off instead of silently swallowing it.

## Delivery

### The ordered check

[`DeliveryCheck`](../../../crates/rimz/src/message/deliver.rs) evaluates a queued record against a fresh snapshot and returns the **first** thing blocking it. The order is the contract: it is what `rimz message show` renders, what the sweep backs off on, and what the delivery helper re-validates before claiming.

| # | Check | Verdict when it fails | What releases it |
| --- | --- | --- | --- |
| 1 | `not_before` has passed | `Scheduled` | the clock, via the elder sweep |
| 2 | every `after` condition stamped | `WaitingOnAfter` | the referenced agent reaching its gate with no ready queued work |
| 3 | every `when` condition stamped | `WaitingOnWhen` | the watched agent completing its dwell in the raw status |
| 4 | oldest deliverable record for this card and lane | `BehindFifo` | the blocking record settling |
| 5 | the receiver card exists in the snapshot | `ReceiverGone` | the agent reappearing, or GC archiving the record |
| 6 | not inside the compaction window | `Compacting` | `CompactionEnded`, or the 90 s window expiring |
| 7 | the gate is open for the effective status | `GateClosed` | the agent reaching `Idle`/`Success`, plus `Failed` under `--on any` |
| 8 | a `Resume` gate's park is genuinely recoverable | `ResumeUnrecovered` | the budget window resetting or the overload marker clearing |
| 9 | no open blocking prompt reserving input | `AskWaiting` | answering the ask, or `--force` |
| 10 | a live pane can receive a paste | `NoPane` | the pane reappearing; affinity is cleared so any pane will do |

All ten pass and the verdict is `Ready`.

`rimz message show` diagnoses `ReceiverGone` against the audit rollup. When the durable receiver card survives but runtime projection expelled it, the verdict names the card's last-seen time, says that no live process claims it, and points at `rimz agents resume`; the delivery verdict and JSON check vocabulary stay unchanged.

Two clarifications the table cannot carry:

**Hook installation is an enqueue check, not a delivery check.** A parked record needs hooks because hooks are the trigger, so dispatch refuses the send up front rather than accepting text it could never release.

**The compaction window at check 6 closes every gate, including `Resume`.** A receiver carrying a `compacting_since` marker inside 90 seconds takes nothing at all. The window expires by design: a lost compaction-end signal degrades to a delay rather than a wedged queue.

`DeliveryGate::Resume` has no flag. Auto-continue stamps it on its own nudge, and check 8 re-verifies at delivery time that the park is still resumable. Ordinary `Done` and `Any` messages stay parked while an agent is paused, which is what keeps a rate-limited agent from receiving a pile of user text the moment it wakes.

Records that are scheduled, condition-blocked, or `Resume`-gated are filtered out of the FIFO scan, so they never block a later record that could deliver now. Resume nudges additionally live in their own control lane, so a wakeup does not queue behind user text that cannot deliver until after the wakeup.

### What triggers a delivery

Three paths, all converging on the same one-message helper:

**Lifecycle hooks.** The lifecycle reactor declares [`DELIVERY_CHECKPOINT`](../../../crates/rimz/src/agents/lifecycle/event.rs) as `TurnEnded`, `TurnInterrupted`, and `CompactionEnded`. On one of those the reactor finds the FIFO head for the event's agent card and spawns a detached `rimz message deliver --message-id <id>` with nulled stdio. `Registered`, subagent stops, and compaction starts do not check the queue.

The same reactor separately nudges the sweep when this agent is *referenced* by an unmet condition: `after` conditions on `DELIVERY_CHECKPOINT`, `when` conditions on the shared [`CONDITION_CHECKPOINT`](../../../crates/rimz/src/agents/lifecycle/event.rs), which additionally includes `Registered`, `TurnStarted`, `AwaitingInput`, and the subagent edges because a dwell can start or break on any of them. Both actions consume the committed `LifecycleEvent`; the ordered delivery helper still re-checks durable state before claiming.

**The elder sweep.** The room's elected sidebar elder reads `message-wake.json` and, when the stamp comes due, spawns `rimz message sweep`. See [Scheduling and wakeups](#scheduling-and-wakeups).

**Auto-continue.** When a persisted park reaches its reset or backoff condition, the producer spawns `rimz agents auto-continue`, which queues a `Resume` message (or redelivers the existing one) and then calls the same helper.

### The delivery helper

`rimz message deliver --message-id <id>` is hidden and never run by hand. Its sequence:

1. **Settle.** Sleep briefly (400 ms, `RIMZ_MESSAGE_SETTLE_MS`) so the agent's state stabilizes after the hook fired.
2. **Re-check.** Run the ordered check against a fresh snapshot. The hook's decision is a hint; this is the decision.
3. **Claim.** Under the workspace lock, move the compatible FIFO batch from `Queued` to `Claimed` and bump each `attempts` in one transaction. Claimed records leave the queued scan immediately.
4. **Write.** Send through the same paste path as `--steer`. If smart compaction fires, write the `/compact` command alone and release the prompt batch back to `Queued` with no attempt penalty.
5. **Record.** A successful write moves the batch to `Sent`, still live until confirmed.

`rimz message steer <id>` reuses the helper with a steer policy: it still requires the named record, a receiver card, and a live pane, but ignores `not_before`, FIFO position, the ordinary gate, and the resume-recovery check. It claims only the named record through `claim_message_for_steer`, keeping the TTL guard and skipping the FIFO compare. This is the manual escape hatch for a dependency cycle or a vanished upstream agent. A waiting ask still defers unless the record or the command carries `--force`.

### Batching

When a queued head delivers, the helper extends the claim through the contiguous ready prefix of that head's lane, so a run of messages that piled up during one turn arrives as one interaction instead of several.

A member joins the batch only if it is a `Prompt` that submits with Enter, does not start with `/`, has its own gate open, matches the head's `force` flag, and shares the head's batch key (the sender's channel for an agent, the receiver's channel for a human, as if typed in the pane). A `Command` body, slash text, a no-enter draft, a force mismatch, a closed gate, or a cross-channel sender stops the batch. Resume control messages never batch.

The batch lands as one paste and one submit. Agent- and human-authored members keep their own structured message header, system members stay verbatim, and sections are separated by a blank line. Claim, `Sent` recording, release, and pre-send failure each mutate the whole batch in one queue transaction, while audit events stay one per message in message order.

### Confirmation and retry

A `TurnStarted` hook aligns the whole submitted paste against candidate `Prompt` batches for that card. Durable record text supplies the boundaries between adjacent messages; agent- and human-authored records consume their structured headers, while system records match verbatim. Record text and the blank-line join match verbatim inside a batch; only the first record's leading and last record's trailing whitespace follow the hook payload's outer normalization. The acknowledgement confirms a batch only when every record accounts for the reported paste; reported text that aligns with no batch is direct pane input and confirms nothing. The correlated path also accepts a requeued prompt until twice its body window after `last_sent_at`, so the record keeps one additional window after reconciliation to absorb a racing acknowledgement instead of allowing another write.

When a turn-start adapter reports no usable prompt text, confirmation falls back to the oldest `Sent` prompt and its `batch_id`, preserving hookless-text compatibility. A `Compacting` hook uses the same oldest-`Sent` fallback for `Command` records. Correlation never selects a `Claimed` record because its deliverer owns an in-progress pane write.

Failure has three shapes:

**Pre-send failure** (pane gone, gate closed, an ask reserving input). Shared recovery keeps or returns the record to `Queued` with `last_error` set and pane affinity cleared, so the next boundary re-resolves a pane. A claim increments `attempts`; an initial send-now failure records the error before any claim exists. An immediate steer rejected on Waiting is terminal and reports the skipped target.

**Unconfirmed prompt** (bytes landed, no hook arrived for 30 seconds by default). The sweep reconciler clears `pane_id` and `batch_id`, preserves `last_sent_at`, increments `unconfirmed_sends`, records `delivery unconfirmed; re-queued`, and retries through the normal FIFO path. A requeued batch member reforms with whatever header is ready next time. Past the cap the record becomes `TimedOut`.

**Unconfirmed command** (bytes landed, no hook arrived for 3 minutes by default). The sweep settles the record `TimedOut` with `delivery unconfirmed; command not resent`. A command reaches the pane at most once because a duplicate `/compact` can discard context and no missing acknowledgement proves the first submit failed.

While the receiver is compacting, the reconciler holds either body in place and pushes its wake hint one body-specific window ahead: confirmation is delayed rather than discarded.

**Neither.** A record whose agent simply has not reached a qualifying boundary is not a failure. It stays `Queued` with no counter moving.

## Writing to the pane

### Paste, then submit

A `Prompt` is wrapped in bracketed-paste markers (`ESC[200~` to `ESC[201~`) through `MuxBackend::paste_text`, then Enter is pressed as a separate `send_key`.

The separation is lexical, not temporal. Agent composers run paste-detection heuristics: text plus a trailing `\r` coalesced into one PTY read is treated as pasted content, with the `\r` a literal newline rather than a submit. Because the composer leaves paste mode on `ESC[201~`, the following Enter is unambiguously a keystroke even when every byte arrives in a single read. On this `Prompt` path, do not try to fix a submit problem by adding a delay; the close marker is what does the work.

Inside the paste, logical LF and CRLF line endings travel as CR. Agent composers normalize CR back to a newline but drop a bare LF; terminal emulators and tmux's own `paste-buffer` likewise send CR for pasted line endings. Multi-line prompts therefore land multi-line. A `Command` body takes the raw type path instead, because a composer treats pasted text as literal content and a pasted `/compact` would land as a prompt rather than run. That path stays byte-faithful and has no paste close marker: composers can reassemble fast typed chars into a synthetic paste, and Codex buffers chars less than 8 ms apart and suppresses Enter for 120 ms after the burst. RimZ therefore waits one command-submit delay (1 s, `RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS`) between raw-typed command text and its Enter keystroke. The public `rimz pane send` also stays on the byte-faithful raw type path, since a bare shell would render the markers literally.

What the send path spaces is *messages*, not the paste and its submit: it sleeps one message interval (1 s, `RIMZ_MESSAGE_INTERVAL_MS`) before each message after the first, so fan-out members and a compact-then-prompt pair reach the composer as separate events.

### The Sent-before-submit barrier

[`write_batch`](../../../crates/rimz/src/message/send.rs) records the batch as `Sent` after the paste lands and **before** it presses Enter. A submitted message is therefore always preceded by its durable record and audit event. The failure ordering this buys: a crash between paste and submit leaves a `Sent` record whose text sits unsubmitted in the composer, which the reconciler can reason about. The reverse ordering would let an agent start a turn RimZ has no record of.

### The message header

Every attributed delivery carries a header before its raw record text:

```text
Type: AGENT_MESSAGE
From: @sender
Content:
<message>
```

`Type` is `AGENT_MESSAGE` for a send from a RimZ-launched agent and `USER_MESSAGE` for a human's `rimz message`; a human header always uses `From: @user`. An agent handle gains `#channel` when the delivery crosses lanes. The recipient's lane comes from its registered channel, its live pane channel, or the addressed channel, so a just-launched same-lane teammate does not gain a spurious suffix before pane capture lands.

The agent handle is the shortest unique selector over addressable agents: role when unique in scope, then explicit launch name, then profile when unique, else kind, else kind ordinal, else pet name. A session rebirth's co-resident audit row is not addressable, so it never pushes the live pane owner's handle down this ladder. System records and `--no-from` sends stay verbatim.

The receiver's turn-start hook parses the header once. `AGENT_MESSAGE` becomes a first-class `Message` transcript entry with structured `from`; `USER_MESSAGE` becomes a `Prompt` entry with the header removed and no `from`. When the agent has an open question, the first human `Prompt` segment instead becomes its id-stamped `Answer`; an agent-authored `Message` never answers it. The queue record supplies the confirmed message id and parentage stamped onto that entry, while the parsed body stays the transcript content.

## Smart compaction

A long turn can hit the context ceiling mid-message. Agents compact on their own only at the ceiling (Codex around 90%), so a prompt sent past it can be cut in half by a compaction that fires mid-turn. `--smart-compact` compacts *first*, so the prompt always lands against a fresh window.

Thresholds parse as `70%` (a fraction of the window), `120000` (absolute occupied tokens), or `180k` / `1m` (suffixed counts). Domain dispatch resolves an omitted threshold from the [`[harness] smart_compact`](../../guide/configuration.md#smart-compaction) config default for every caller, including scheduled loop wakes. An unknown fill never triggers: a missing reading is not a full window, so the text sends untouched.

A percent threshold reads the same fill gauge the sidebar card renders (`context_fill_pct`); a token threshold reads `occupied_context_tokens`, which prefers the folded statusline breakdown, then the per-call split (cache reads plus cache writes plus fresh input), then the carried `total_tokens` gauge.

Two paths, because the modes have different promises:

- **Boundary.** Send the tracked `/compact` command alone, release the claimed prompt batch back to `Queued` with no attempt penalty, and let `CompactionEnded` start a fresh delivery against the new window. A parked record re-reads fill at the boundary rather than trusting the reading from enqueue time.
- **Steer.** Keep immediate semantics: type `/compact`, then one message interval later paste the prompt into the composer. Reconciliation holds that `Sent` prompt in place while compaction delays its confirmation.

`compacted_context_tokens` records the reading a compaction fired on. While a carried-forward stale gauge still equals that baseline, the send path suppresses duplicate `/compact` commands; a new reading re-enables it. Without this a stuck gauge would compact on every message.

A failed compaction fails the delivery through the same retry path as a failed send, which is the point of routing the command through a real record rather than typing it inline.

## Idle compaction

Idle compaction reuses the command-record half of smart compaction without attaching a following prompt. The elected sidebar producer checks top-level rollup agents against `[harness] idle_compact`, the idle threshold, the 50,000-token floor, adapter support, and the `auto` re-engagement signals, then spawns a detached `rimz agents idle-compact` helper so the sidebar import graph remains read-only on the store.

The helper re-resolves the workspace, session, and pane, verifies that the agent is still idle and the configured threshold is still due, and validates the command against the adapter. It queues one automated system `MessageBody::Command` with `DeliveryGate::Done`, pins the pane, stamps `compacted_context_tokens` from the fresh context reading, and attempts `DeliveryPolicy::Boundary`. A closed boundary stays queued through the ordinary retry disposition rather than typing into a working, waiting, parked, or compacting pane.

Three layers make the action once-per-idle-stretch. The producer's cache-class `(kind, agent_id)` record suppresses the same `last_activity` and throttles helper respawns; the live queue and `last_compact_command_tokens` suppress the same occupied reading; and the helper stops when the session's latest delivered record is already a compact command. A later delivered prompt breaks that final guard and begins a new eligible stretch once its own activity and context thresholds are reached.

## Reply waits

`--wait[=DURATION]` turns a send into a synchronous scatter-gather. It stamps `reply_wait` on each durable record, then polls until every leg settles.

Dispatch captures one frame-aligned event-log base before enqueue and copies it into every leg, so each leg folds terminal message events from its own private cursor. Each `Sent` leg anchors a skip-existing transcript cursor before dispatch, so it retains only assistant messages written after the prompt.

Each leg is a two-phase machine:

- **Delivery.** Waits for `Delivered`, stamped by the prompt's own `TurnStarted`. A steer into an already-running turn opens the next phase from `Sent + Running` instead, because the interrupted turn emits no second `TurnStarted`: the remainder of that turn *is* the reply.
- **Reply.** `Idle` or `Success` completes the leg; `Failed`, a delivery failure, a vanished card, or a skipped Waiting input fails it while the other legs keep gathering. `Waiting` and `Paused` stay inside the reply. A changed `turn_started_at` while the card is still `Running` proves the reply turn ended and another began between polls.

One 500 ms poll reads the message list and cached snapshot once per tick and advances every unfinished leg. On entry and every tenth tick it also folds agent-context sidecars and re-checks for dependency cycles.

### Cycle detection

Two agents can each wait on the other and hang forever. The module builds a wait graph and refuses to enter one.

A live edge runs from a named, `Running` sender card to the receiver of a `Queued`, `Claimed`, or `Sent` `reply_wait` record. A delivered wait stays an edge while its record id appears in the receiver's `turn_opened_by` context, with terminal records read from history. Senders that are unnamed, missing, or not `Running` contribute no edge.

Dispatch checks the graph before enqueue, and the periodic poll closes the race between simultaneous dispatches or cycles that form later. The **youngest** `msg_` id in a detected cycle yields: only that reply leg is marked failed, and its durable text is left untouched so the turn boundary still delivers it. Older waits continue. Diagnostics name the blocking handle and message id and render the multi-hop chain.

### Settlement

The join succeeds only when every leg completes; otherwise it takes the first non-completed status in target order. `--any` returns the first terminal leg without canceling the rest.

One deadline spans fan-out dispatch and every reply turn. A human's bare wait is indefinite; an agent-authored bare wait defaults to one hour as a backstop; an explicit duration wins. Expiry marks each unfinished `Sent` record timed out, classifies every unfinished leg as `TimedOut`, and exits 124.

Text output streams labeled successful replies in completion order; `--json` buffers one settled handle-keyed map. A single agent that emits no assistant message yields empty stdout plus a stderr note.

Preparation requires lifecycle-bound cards with installed, trusted hooks, deduplicated per kind. It accepts broadcasts and fan-out; it rejects create-on-miss, schedules, `--after`, `--when`, bare pane targets, and unsubmitted pastes before dispatch.

## Channels

Every target lives in a channel: the cooperation lane inside one room, the identity the sidebar groups by, the `#channel` suffix an address takes, and the tab name RimZ recovers on rebirth.

### Where a lane comes from

Launch resolves one lane and stamps it into `RIMZ_CHANNEL`, the launch event, and the rollup. [`resolve_room_channel`](../../../crates/rimz/src/harness/target.rs) takes the first that applies:

1. An explicit `--channel`.
2. The current directory's basename, whenever the agent runs below the project root. A RimZ-owned worktree is the common case; any nested checkout works the same way.
3. `<dir>/<team>` for a named team launched in place at the room root.
4. Nothing, for a bare directory room.

Read paths use the stamped lane, falling back to the worktree basename only for agents that carry no stamp. Worktree identity follows the agent's own resolved checkout rather than the room tree: hooks resolve the git toplevel from the agent's cwd at any depth, so an agent working a nested checkout gets that checkout's lane, while non-git agents at the room root fold into the room's root lane.

Lane equality scopes target resolution, rendered handles, sidebar grouping, `agents list`, pane overlays, message list, transcripts, and recovery. Branch names stay display metadata on the worktree card; they never define lane identity.

### The registry

`channels.json`, beside `workspace.json` in the workspace store, holds **only** bare named channels: a name and a creation time, written under the workspace lock with temp-file-plus-rename. Worktree lanes use their `rimz-worktree.json` marker as durable truth, and team and directory lanes derive from the stamped launch identity. `rimz channel list` unions all three.

The sidebar stays presence-driven, so a group appears only when a pane runs in that lane. An empty named channel still persists, still lists, and still reopens as an empty tab on rebirth. Named-channel records survive until `rimz channel rm`; `rimz gc` acts on worktrees only.

Named channels and RimZ-owned worktrees share one namespace. `rimz channel new NAME` refuses an existing worktree channel and `rimz worktree new NAME` refuses an existing named channel, each naming the other command as the fix.

### Addressing into a lane

Commands run inside a stamped pane inherit `RIMZ_CHANNEL`, so `@claude` scopes to that lane. A human shell in a bare directory room has no current lane and reaches the whole room; `message list` treats that as the main-lane inbox, and `--all` widens it.

`--worktree` and `--channel` are separate launch intents: a worktree launch creates or reuses Git backing, while a named-channel launch stays in the room root and records only the bare lane. Inline `#design` and `--channel design` reconcile through the same target parser, so a mismatch fails before delivery.

## Transcript

Routing text is the write side. The transcript log is the durable record of the resulting conversation, and `rimz transcript` reads it back as a chat timeline.

The log is RimZ-owned and distinct from a provider's native session files, so ended agents, past channels, and their asks and answers stay readable after those native files rotate away.

Hook and delivery paths append to fixed 7-day buckets at `transcript/<bucket-start>.jsonl`, append-only, under the workspace lock. Buckets are never pruned, and reads sort by recorded timestamp, so a bucket boundary carries no ordering meaning.

| Kind | Records | Reads back as |
| --- | --- | --- |
| `Prompt` | a human prompt when no question is open | `user: @receiver, text` |
| `Message` | an inter-agent delivery, with structured `from` | `@sender: @receiver, text` |
| `Assistant` | a root turn's final assistant message | `@receiver: text` |
| `Ask` | a native question, when a blocking hook marks the agent waiting; `questions` carry option labels and descriptions | the agent's question |
| `Answer` | the effective answer from the native prompt UI or the first human prompt submitted while a question is open | `you` to the agent, folded into its ask card |
| `Error` | a hook-path provider error newly merged into `AgentContext.turn_error` | `@receiver: error text`, styled as an error |

### Causality

Two fields link entries into conversations, and both default empty so older JSONL lines decode unchanged.

`message_id` stamps a delivered prompt with its queue record. Confirmation returns every record in the submitted batch; alignment restores each reported section from the durable record boundaries, then each section matches a returned record by exact body text in order. Hand-typed prompts and unmatched text carry no linkage.

`reply_to` carries the parent message ids. The same turn-start replaces `AgentContext.turn_opened_by` with every matched id, including an empty vector that clears a prior turn. An agent-authored enqueue copies that current-turn vector into its new record's `in_reply_to`; a human sender, `--no-from`, an unnamed sender, or missing context starts a root. The turn's final `Assistant` entry and any mid-turn `Ask` copy `turn_opened_by` into `reply_to`. Requeue preserves `in_reply_to`, so retrying text keeps its causal position.

A batched delivery splits on blank-line boundaries that introduce another `Type: AGENT_MESSAGE` or `Type: USER_MESSAGE` header, so each section becomes its own entry. A provider turn-error becomes an `Error` entry only on the hook-path merge (`StopFailure` or a `Stop` tail refresh); statusline-only detections stay card enrichment, because that path is lock-free and writes no transcript.

`rimz transcript` projects linked entries into flat conversation components: it unions output edges from an `Assistant`, `Ask`, `Error`, or `Answer` to the messages that opened its turn, plus reply-back edges from a message to a parent whose sender is that message's receiver. Other causal edges, including hand-offs to third parties, root new conversations. The earliest entry in a component is its root; the rest follow chronologically beneath it. `--flat` skips the assembly.

The reader computes a current-life boundary at read time: the earliest `registered_at` among the matching live root agents. Earlier entries are prior-session archive, hidden by default when a live cohort exists, rendered under a dated marker with `--all`, and rendered wholesale as archive when no live cohort exists. The buckets themselves are never mutated.

Three nearby reads are not this log. Supervised-run streaming tails the provider-native transcript through the adapter-owned source ([scripting.md § Output and input projections](./scripting.md#output-and-input-projections)); the context and spend gauges read those same native stores ([model.md § Enrichment](../agents/model.md#enrichment)); and the audit trail below carries no message text at all.

## Asks and answers

An agent holding a permission prompt, plan approval, or question reserves its input, which is check 9 in the delivery order. The ask machinery is how that reservation becomes structured data.

A blocking hook mints an `ask_` id at ingestion and writes it onto the `AwaitingInput` signal. The reducer projects it to `AgentState.open_ask` and clears it on the same edges that clear `waiting_since`. Old events without an id still replay as waiting rows, just without a structured ask.

Question and plan hooks append a transcript `Ask` entry carrying the same id, parsed questions, and the agent's ask-time assistant text. Permission hooks keep their short tool summary on `open_ask` and synthesize the adapter's safe options at read time, which avoids recording a transcript ask that has no native closing answer event. A later turn-start hook classifies its first human prompt as an id-stamped free-text answer when that question remains open; this closes the durable ask even when the provider emitted no native answer event. `rimz asks` treats `is_awaiting_input` plus `open_ask` as truth and joins the parsed questions and assistant text by id only, exposing that text as `context`.

`rimz answer` validates every selector before touching the pane, then re-reads the rollup and requires the target id to still be the agent's current open ask. That compare-and-swap is what stops a stale bridge response from answering a newer prompt. The Claude adapter maps user-question answers to native keys and paste actions, permission `allow` to digit 1, and plan `approve` to Shift-Tab. Each permission or plan ask lists only that single confirmable action; Escape-based rejection, persistent grants, refinement text, and manual-review approval fail before delivery and name the pane as the place to do it.

Confirmation polls until the ask leaves the rollup or a matching transcript `Answer` appears. A confirmed command appends the structured answer only when the native PostToolUse path has not already recorded that id. `--no-wait` skips both, rather than claiming the pane accepted bytes it never acknowledged.

## Scheduling and wakeups

A parked message needs someone to notice it came due. That someone is the room's elected sidebar elder, and the handoff is deliberately thin.

1. The CLI writes `message-wake.json` under the runtime root with the earliest interesting future timestamp: a `not_before`, a `Queued` retry floor, a ready-queued backstop, or an unconfirmed `Sent` reconcile deadline. Prompt deadlines use `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` (30 s by default); command deadlines use `RIMZ_MESSAGE_COMMAND_DELIVERY_WINDOW_MS` (3 minutes by default).
2. The elder reads only that file. When the stamp comes due it spawns a detached `rimz message sweep` ([`fire.rs`](../../../crates/rimz/src/message/fire.rs)). No store reads, no store writes, no message logic in the elder.
3. The sweep reconciles stale `Sent` records, evaluates unmet conditions, delivers ready FIFO heads, then rewrites the wake stamp or removes it.

The sweep is single-flight through a `message-sweep.lock` file lock, so overlapping wakeups collapse into one pass.

Condition evaluation inside a sweep is one transaction: it evaluates every unmet condition against one context-enriched snapshot, applies every stamp, retry floor, and watched-agent archive together, reloads the pending records, and delivers newly eligible heads from that same snapshot in the same run. New stamps emit `message.after_met` or `message.when_met`.

Backoff matters here, because the elder ticks often. When a sweep cannot deliver a ready head (gate closed, ask waiting, compacting, no pane) it writes `retry_after = now + RIMZ_MESSAGE_DELIVERY_WINDOW_MS` (30 s by default), so the elder retries at most once per delivery window instead of every tick. `retry_after` is a wake hint and nothing more: it does not affect `is_ready`, FIFO position, claim leases, or hook-driven delivery.

A ready `Queued` head arms the stamp even with no `not_before` at all, contributing its `updated_at`. That backstop is what recovers a message to an idle agent that missed the live send path.

An unmet `when` condition sets `retry_after` to the exact projected trip time rather than a flat window, so a 58-minute dwell wakes once at 58 minutes. An ended watched session archives every still-unmet record with the condition in `last_error`; the lifecycle hook is the realtime path and orphan GC is the durable backstop. A met stamp survives session end and receiver delay: latching is what lets a busy receiver still get the message at its next boundary.

Durations accept `s`, `m`, `h`, `d`. Wall-clock `HH:MM` resolves to the next occurrence in the configured `timezone` (today if still future, else tomorrow), falling back to the system zone. Zero durations are rejected.

## Storage and audit

```text
messages/messages.jsonl   live Queued, Claimed, and Sent records
messages/history.jsonl    terminal records, with text, newest 500 kept
events.log.jsonl          terminal message.* audit events, without text
```

The queue file holds only live records. A terminal transition appends the final record to history, removes it from the queue, then appends the audit event. History is pruned after append once it passes 512 KiB, rewriting the newest 500 records in `msg_` order. All writes hold the workspace lock; queue rewrites use temp-file-plus-rename, history uses the append helper. The queue file is created lazily, so an empty workspace costs the hook path one missing-file stat.

The store exposes `list()` (live), `list_history()` (terminal, with text), and `list_pending()` (`Queued` only). A missing queue file means no live records, not an error.

Every status transition appends one typed event: `message.queued`, `message.edited`, `message.after_met`, `message.when_met`, `message.sent`, `message.delivered`, `message.timed_out`, `message.errored`, `message.canceled`, `message.abandoned`, `message.archived`. The parser still reads legacy `message.removed` as a cancellation.

The payload carries `message_id`, `address`, `kind`, `agent_id`, `agent_name`, `channel`, `gate`, `status`, `body`, `pane_id`, the `forced` flag, sender attribution, `text_len`, both attempt counters, timestamps, the compaction baseline, and a `reason` on error or abandon. **Message content never enters the event log.** That is why `message list` merges three sources: live records, history records with text, and old or unresolved terminal rows from events without text.

## The inbox verbs

Flags and rendering are [cli/message.md](../../reference/cli/message.md). What they do underneath:

- `message list` / `message show` merge the three sources above. The rendered handle comes from the record's enqueue-time `address` first, then the live snapshot, then `agent_name` plus channel, then `kind:agent_id`. `show` renders the ordered delivery check, so it names the *first* unmet condition rather than a list of everything.
- `message edit` is the single compare-and-swap path for a queued record. It accepts only `Queued`, refuses `Claimed` as in-flight, reports terminal records from history, applies the delivery deltas, clears `retry_after` so the next sweep sees the change, and appends `message.edited` naming the changed fields. Receiver identity, channel, card, sender, and pane affinity stay outside edit: retargeting is cancel plus send.
- `message steer` pushes a queued record through now, bypassing schedule, FIFO, and gate.
- `message requeue` copies a terminal history record into a fresh `Queued` record with a new id, preserving text, receiver, channel, sender, body, delivery settings, and `in_reply_to`, while clearing condition stamps. Event-only terminal rows cannot requeue, because their text was never in the event log.
- `message cancel` settles named live records; `message clear <target>` settles every open record for one card, and targetless `message clear` settles the scoped lane.

Two hidden helpers are the pipeline's arms, spawned detached with nulled stdio: `message deliver --message-id <id>` and `message sweep`.

## Hazards

- **Queued text can land on a half-typed human draft.** RimZ gates on store state, never on focused-pane state or captured composer contents. Reading the composer to decide would make delivery depend on a pane read, which is latency, not truth.
- **Agent UIs can show dialogs no hook reports.** Core keeps pane capture out of delivery. A script that must inspect UI text owns capture-before-send through the public `pane capture` primitive.
- **Multiplexer writes are best-effort.** A pane can vanish or reject input after the claim. The record absorbs that: it stores the error and retries to the cap.
- **A pane write is not an acknowledgement.** Treat `Sent` as pending. Anything that assumes the agent read the text belongs after `Delivered`.
