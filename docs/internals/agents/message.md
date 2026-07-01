# The message system

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — rollup, state machine, turn phase, liveness — is [agent.md](./agent.md); addressing and the exec wrapper are [harness.md](./harness.md); channels are [channels.md](./channels.md); the user-facing commands are [cli/agents.md](../../reference/cli/agents.md). This doc owns the machinery that delivers text to an agent: send modes, the durable message record, delivery gates and FIFO ordering, the hook-triggered delivery pipeline, scheduling, smart compaction, wait confirmation, retries, and the audit trail.

`rimz message` is the teammate chat surface. A human, a script, a CI hook, or another agent names a target with the [address grammar](./harness.md#the-address) and Rimz delivers text through the same pane-send primitive that resolvers and the `pane send` public command share. One command owns the whole timing axis: `--steer` sends to a live pane now, the default sends now only when the target can receive and otherwise parks a durable record, and `--schedule` adds a `not_before` floor before the usual gate opens.

## Send modes

Three modes cover the timing axis. All three resolve the target through the same address parser, ride the same bracketed-paste primitive, and write the same audit events.

- **`--steer`** — interrupt the live pane immediately. Writes a durable `sent` record and prints `sent to @handle (msg_...)`. Conflicts with `--schedule` and `--on`, because it has no later boundary.
- **Default** — send now when the target has a live pane, the gate is open, no pending ask reserves input unless `--force`, and no older ready queued message owns that card's FIFO head. When any condition fails, the text parks as a `queued` record for the next qualifying turn boundary. A successful send-now writes the same durable `sent` record as `--steer`.
- **`--schedule <DUR|HH:MM>`** — always parks and stores a `not_before` timestamp. The room must be open so the sidebar elder can spawn `message sweep` when the wake stamp comes due.

## The message record

Each message is a durable JSON file written with temp-file-plus-rename through the ledger atomic helpers. `msg_` ids are short workspace-unique time-sortable tokens, so filename order is FIFO order; queued scans read only `messages/*.json`, and the directory is created lazily so an empty workspace costs the hook path one missing-dir stat.

```text
messages/<msg_id>.json            queued or claimed (open)
messages/terminal/<msg_id>.json   sent, delivered, or other terminal state
```

A record stores:

| Field | Purpose |
| --- | --- |
| `message_id` | `msg_` prefixed, time-sortable |
| `workspace_id` | owning workspace |
| `kind`, `agent_id`, `agent_name` | receiver identity; name enables provisional-to-registered FIFO folding |
| `channel` | receiver channel at enqueue time |
| `sender` | `Human` or `Agent { kind, name, profile, role, channel }` — for attribution, never the body |
| `body` | `Prompt` (default) or `Command` (a `/compact` or adapter command) |
| `text` | the message content |
| `enter` | whether to submit with Enter after the paste |
| `gate` | `Done` or `Any` — the turn-boundary statuses that release delivery |
| `force` | deliver past a pending ask |
| `pane_id` | pane address when known at enqueue time |
| `status` | lifecycle state (see below) |
| `enqueued_at`, `updated_at`, `delivered_at` | timestamps |
| `attempts`, `last_attempt_at`, `last_error` | retry bookkeeping |
| `not_before` | earliest delivery time for scheduled messages |
| `auto_compact` | context-fill threshold that triggers a `/compact` before delivery |
| `compacted_context_tokens` | baseline reading that suppresses duplicate compaction |

The full record is the field catalog; the lifecycle below is the contract. Domain model: [`message.rs`](../../../crates/rimz/src/message.rs).

## Message lifecycle

```text
Created ──► Queued ──► Claimed ──► Sent ──► Delivered
               │          │          │
               │          │          ├──► TimedOut
               │          │          └──► Errored
               │          └──► (revert to Queued on pre-send failure)
               │
               ├──► Removed    (user)
               ├──► Archived   (receiver ended, channel teardown)
               └──► Abandoned  (retry cap exceeded)
```

- **`Created`** is transient — the record reaches `Queued` before the write returns.
- **`Queued`** and **`Claimed`** are **open** (`is_open`): the message is live in the queue.
- **`Sent`** means bytes were written to the pane; the record has left the pending queue (`leaves_pending_queue`) but is not yet terminal.
- **`Delivered`** means the agent acknowledged the text — `TurnStarted` for a `Prompt`, `Compacting` for a `Command`.
- **Terminal** states (`Delivered`, `TimedOut`, `Errored`, `Removed`, `Abandoned`, `Archived`) are final; the record relocates from `messages/` to `messages/terminal/`.

## Send path

### Bracketed-paste submit

Immediate sends wrap the text in bracketed-paste markers (`ESC[200~` … `ESC[201~`) through `MuxBackend::paste_text`, then press Enter as a separate `send_key`. The boundary is lexical: agent composers run paste-detection heuristics — text plus a trailing `\r` coalesced into one PTY read is taken as pasted content, with the `\r` a literal newline rather than a submit — so the composer leaves paste mode on `ESC[201~` and the following Enter is unambiguously a keystroke even when every byte arrives in one read.

The discrete writes land one second apart after the first write: paste immediately, wait, submit. This gives a busy composer separate paste and submit events on the PTY. `\n` inside the text rides the paste as a soft composer newline, so a multi-line prompt lands multi-line. The generic `rimz pane send` stays on the raw type path, since a bare shell would render the markers literally.

### Sender prefix

By default a Rimz-launched agent's send arrives prefixed `from @sender: `, gaining `#channel` when it crosses channels. The recipient lane comes from its registered channel, live pane channel, or addressed channel, so a just-launched same-lane teammate does not gain a spurious suffix before pane capture lands. The handle uses the shortest unique selector: the role when unique in scope, then the profile when unique, else the kind, else the petname. `--no-from` delivers without the sender prefix. `parse_sender_prefix` is the shared inverse used by transcript reads and the chat-log build ([harness.md § Read the room](./harness.md#read-the-room)).

A fan-out also prefixes the text with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message.

### Fan-out

A multi-match is an ambiguity error until `--all` or `@all` opts in. Fan-out delivers to every match, prefixes each delivery with the addressed handle, skips a blocked agent while the rest send, and paces deliveries one message interval (1 s default, `RIMZ_MESSAGE_INTERVAL_MS` overrides) apart between pane writes. Broadcasts summarize sent and skipped agents with handles and message ids.

## Targets

`message --steer` reaches **live panes**: a bare `@<kind>` or `@all` also reaches a pane that has not bound a session yet — a lazy-registering agent (Codex) before its first turn ([agent.md § The instance lifecycle](./agent.md#the-instance-lifecycle)) — because the thing a paste needs is the *pane*, which the producer already detects.

The default message path uses that live pane when the target can receive now, including lazy panes with no session yet; when it must park work, it keys the durable record on the bound session or launch placeholder card so FIFO survives registration. A message queued against a provisional `launch_*` card keeps the launch id in the record; when the card registers, name-based matching (`same_card`) folds it into the session's single FIFO queue — one card, one queue.

A petname, kind ordinal, or real session-id prefix names a bound session in every mode; launch placeholder ids stay internal. The `@` sigil is required — a bare selector fails with a `did you mean @…?` hint, so a stray word never broadcasts; a pane id is the one sigil-free exception. Floating Zellij panes participate in live-pane addressing.

## Gates and delivery conditions

A parked message delivers when all five conditions hold:

1. **Gate is open.** `DeliveryGate::Done` opens on `Idle` or `Success`; `DeliveryGate::Any` also opens on `Failed`. `Running`, `Waiting`, and `Paused` keep delivery closed.
2. **No pending ask.** A feed ask attached to the agent's bound session reserves the next input. `--force` bypasses the ask, mirroring `message --steer --force`.
3. **FIFO head.** The message is the oldest *ready* queued record for its card. `msg_` id string order is FIFO order; scheduled messages whose `not_before` is still in the future are filtered out, so they never block a later ready message on the same card.
4. **Live pane exists.** The target must have a pane that can receive a paste.
5. **Hooks are installed and trusted.** Parked delivery needs hooks, because hooks are the delivery signal.

`--on done` (the default) and `--on any` set the gate; `--steer` has no gate because it sends immediately.

## Delivery pipeline

### Park path

`queue_message` writes the record to `messages/<msg_id>.json`, appends a `message.queued` audit event, and wakes sidebars. The `messages/` directory is created lazily so an empty workspace costs the hook path one missing-dir stat. Each write holds the workspace lock and uses temp-file-plus-rename.

### Delivery trigger

Only **unparked root turn ends** trigger parked delivery — `Registered`, subagent stops, compaction events, and parked background turn ends (`TurnEnded { parked_on_background: true }`) do not check the queue. The lifecycle hook records the event, loads pending messages, finds the FIFO head for the agent's card, and spawns a detached `rimz message deliver --message-id <id>` helper with nulled stdio.

### The deliver helper

The helper follows a strict sequence:

1. **Settle** — wait a short delay (400 ms default, `RIMZ_MESSAGE_SETTLE_MS` overrides for tests) for the agent state to stabilize.
2. **Candidate check** — read the queued head, verify `not_before` has passed, gate is open against a fresh snapshot, pending-ask predicate holds (skipped under `force`), and a live pane exists.
3. **Claim** — under the workspace lock, transition the record from `Queued` to `Claimed` and increment the attempt count. The claim moves the record out of the queued scan immediately before sending.
4. **Send** — write text to the live pane through the same bracketed-paste path as `--steer`. Smart compaction prepends a fresh `Command` record at delivery time before the claimed prompt.
5. **Settle to terminal** — a successful pane write moves the record to `Sent`.

### Delivery confirmation

The agent's next body-matching lifecycle hook confirms the oldest `Sent` record for that card:

- `TurnStarted` confirms a `Prompt` body → `Delivered`.
- `Compacting` confirms a `Command` body → `Delivered`.

One cannot confirm the other. A smart-compact send owns two records: the `/compact` command confirms on `Compacting`, and the prompt confirms on `TurnStarted`.

### Retry and failure

- **Pre-send failure** (pane gone, gate closed, pending ask blocks): revert the record to `Queued` with `last_error` and the claim timestamp as throttle. The next qualifying turn boundary retries.
- **Post-send failure** (bytes were written but confirmation never arrives): the record becomes `Errored` to avoid duplicate retry text.
- **Retry cap**: after `MAX_DELIVERY_ATTEMPTS` (5) the record becomes `Abandoned`.
- **Claim TTL**: a `Claimed` record older than 15 s (`CLAIM_TTL`) is treated as expired, so a crash after claim leaves a redeliverable record. `message list --all` surfaces it.
- A state miss — the message is queued but the agent has not reached a qualifying boundary — leaves the message queued for a later transition.

### Terminal transitions

| Trigger | Terminal status |
| --- | --- |
| Agent's next lifecycle hook confirms the body | `Delivered` |
| Delivery window expires on a `Sent` record | `TimedOut` |
| Pane write fails after bytes were written | `Errored` |
| User runs `message remove` | `Removed` |
| Retry cap exceeded | `Abandoned` |
| Receiver session `Ended` or channel teardown | `Archived` |

Lifecycle `Ended` archives receiver messages in realtime; worktree create/remove archives records keyed to that channel; `rimz gc` is the durable backstop and times out `Sent` records older than `RIMZ_MESSAGE_DELIVERY_WINDOW_MS`. `Archived` is distinct from retry exhaustion (`Abandoned`) and explicit user removal (`Removed`).

## Scheduling

`--schedule <DUR|HH:MM>` always parks and stores `not_before`. Durations accept `s`, `m`, `h`, and `d`; wall-clock times resolve to the next occurrence in the configured `timezone` (today if still in the future, otherwise tomorrow), falling back to the system zone when unset. A zero duration is rejected.

FIFO scans filter out messages whose `not_before` is still in the future, so a scheduled message cannot block a later ready message on the same card. The FIFO head is the oldest **ready** queued record for that card.

Scheduled messages need an open room for wakeups:

1. The CLI writes `message-wake.json` under the runtime root with the earliest future `not_before`.
2. The elected sidebar elder reads that cache and, when due, spawns a detached `rimz message sweep`.
3. The sweep helper finds ready FIFO heads whose gates are open, calls the same one-message delivery path as lifecycle hooks, then rewrites the wake cache to the next future schedule or removes it.

Past-due-but-blocked messages (closed gate, pending ask) are not kept in the wake stamp, so a blocked scheduled message does not spawn a helper every tick; the next turn-end hook and `gc` backstop own them.

## Smart compaction

`--smart-compact <PCT|TOKENS>` lands a message against a fresh context window. When the agent's context fill has reached the threshold, Rimz sends a tracked `/compact` command message first, waits one message interval, then sends the prompt message so it runs after compaction instead of racing the agent's own auto-compaction mid-turn.

**Threshold forms:**

- `70%` — a percentage of the context window; fires when `context_pct >= 70`.
- `120000` — an absolute occupied-token count; fires when occupied tokens >= 120 000.

An omitted flag falls back to the [`[harness] smart_compact`](../../reference/configuration.md#smart-compaction) default. An unknown fill never triggers — a missing reading is not a full window, so it sends untouched.

**Reading sources:** the folded statusline reading where present (`context_pct`), else the per-call token split (`cache_read_input_tokens + fresh_input_tokens`), else the carried `total_tokens` gauge.

**Compact-first path:** the `/compact` command rides the raw type path — **not** the bracketed paste — because a composer treats pasted text as literal content, so a pasted `/compact` would land as a prompt rather than run. The compact-first path paces `/compact`, its submit, the message, and its submit one second apart after the first write so compaction settles before the message arrives.

**Baseline tracking:** `compacted_context_tokens` records the token reading the trigger fired on. While a carried-forward stale gauge still equals this baseline, the send path suppresses duplicate `/compact` commands; a new reading re-enables compaction.

**Parked records** store the threshold in `auto_compact` and re-read fill at the delivery boundary, typing `/compact` ahead of the message in the same delivery so a failed compaction fails the delivery through the same retry path as a failed send.

## Wait

`--wait[=DURATION]` upgrades `message --steer` and send-now default messages from fire-and-return to synchronous confirmation. The command waits until the prompt record reaches `Delivered`, `TimedOut`, `Errored`, `Removed`, `Abandoned`, or `Archived`, prints the matching terminal status with handle and message id, and exits nonzero unless delivered. Bare `--wait` uses `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` or the default delivery window (30 s). It conflicts with `--no-enter`, because an unsubmitted paste cannot be confirmed.

**Broadcast waits** share one deadline across all prompt records.

**Smart-compact waits** track two records: the `/compact` command confirms on `Compacting`, and the prompt confirms on `TurnStarted`; one cannot confirm the other.

**Edge cases:**

- `--force` sent mid-turn can time out because a resumed turn emits no fresh `TurnStarted` for that paste.
- A sessionless lazy pane confirms only after a real session or name can match its pane-derived placeholder record, so the first prompt can time out even when the paste succeeds.

## Message store

The ledger persists message records in two directories under the workspace state root:

```text
messages/<msg_id>.json            open (queued or claimed)
messages/terminal/<msg_id>.json   terminal (delivered, timed_out, errored, removed, abandoned, archived)
```

Records relocate from `messages/` to `messages/terminal/` on any status transition out of `is_open()`. Pending scans read only `messages/*.json`, so terminal relocations keep the scan O(1) in the number of active messages. All writes use temp-file-plus-rename through the ledger atomic helpers and hold the workspace lock.

`list` operations span both directories; the store exposes `list()` (all records) and `list_pending()` (open records only). Store implementation: [`ledger/message_store.rs`](../../../crates/rimz/src/ledger/message_store.rs); ledger mutations: [`ledger/writer/queue.rs`](../../../crates/rimz/src/ledger/writer/queue.rs).

## Audit trail

Every status transition appends a typed event to `events.log.jsonl`. The event methods are:

`message.queued` · `message.sent` · `message.delivered` · `message.timed_out` · `message.errored` · `message.removed` · `message.abandoned` · `message.archived`

The payload carries `message_id`, `kind`, `agent_id`, `gate`, `status`, `body` (Prompt or Command), `pane_id` (when known), `forced` flag, `sender` attribution, `text_len`, `attempts`, and `reason` (on error or abandon). **Message content stays in the message record, never in the event.**

## Subcommands

**User-facing:**

- `message list` — defaults to the current channel, hides archived records, sorts newest first, renders `ID STATUS TARGET FROM CREATED DELIVERED TEXT`. `--all` widens the view, `--channel <NAME>` selects one channel, `--status <STATUS>` filters exactly, `--json` keeps the full record including attempts.
- `message status <msg_id>` — prints one record as key/value detail: status, target, sender, channel, timestamps, attempts, last error, and text preview.
- `message remove <msg_id>` — removes a queued or claimed message → `Removed`.
- `message clear <target>` — removes every open message for one agent card. Accepts `--worktree` and `--channel` for scoping.

**Hidden helpers** (spawned by hooks and the sidebar elder, not for human use):

- `message deliver --message-id <id>` — the detached delivery subprocess spawned by lifecycle hooks at unparked root turn ends.
- `message sweep` — the detached helper spawned by the sidebar elder when a scheduled wake stamp comes due; finds ready FIFO heads, delivers, rewrites the wake cache.

## Hazards

- Queued text can land while a human has half-typed a draft in the agent pane. Rimz gates on ledger state, not focused-pane state or captured composer contents.
- Agent UIs can present dialogs that are not feed asks. Core keeps pane capture out of delivery; a resolver that needs to inspect UI text owns capture-before-send.
- Multiplexer sends are best-effort: a pane can disappear or reject input after the claim, which the message record records and retries until the attempt cap.
