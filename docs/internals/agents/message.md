# The message system

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent model (rollup, state machine, turn phase, liveness) is [agent.md](./agent.md); the address grammar and the exec wrapper are [harness.md](./harness.md); the Git worktree backing is [worktree.md](./worktree.md); the user-facing commands are [cli/agents.md](../../reference/cli/agents.md) and [cli/channel.md](../../reference/cli/channel.md). This doc owns how Rimz routes text to a running agent: the send modes, the durable message record, delivery gates and FIFO ordering, the hook-triggered delivery pipeline, scheduling, smart compaction, wait confirmation, retries, the channel lanes that scope addressing, the transcript read-back, and the audit trail.

`rimz message` routes text to a running agent. A human, a script, a CI hook, or another agent names a target, and Rimz types the text into that agent's pane through the same bracketed-paste primitive the public `pane send` command and resolvers use.

One model runs underneath every send: deliver now when the agent can take the text, otherwise park it as a durable record and deliver at the agent's next turn boundary, oldest first. Parking is what lets a message outlive a busy agent, a room that closes and reopens, or a crash between claim and send. A parked message is a record in the room ledger, and delivery is a later step that a turn-boundary hook drives.

Three send modes place a message on that timing axis: `--steer` interrupts the live pane now, the default sends now when the target can receive and parks otherwise, and `--schedule` parks until a wall-clock time. [Send modes](#send-modes) has the detail.

A target is an @-mention resolved against the live fleet: `@claude` names the Claude in the current lane, `@planner#design` names the planner in the `design` channel, `@all` broadcasts. Every target lives in a channel, the cooperation lane that scopes addressing and sidebar grouping; the lane model is [Channels](#channels) below, and the address grammar is [harness.md § The address](./harness.md#the-address).

## Send modes

Three modes place a send on the timing axis. All three resolve the target through the same address parser, ride the same bracketed-paste primitive, and write the same audit events.

- `--steer`: interrupt the live pane immediately. Writes a durable `sent` record and prints `sent to @handle (msg_...)`. Conflicts with `--schedule` and `--on`, since it has no later boundary.
- Default: send now when the target can receive, meaning a live pane, an open gate, no pending ask reserving input (unless `--force`), and the FIFO head of its card. [Gates and delivery conditions](#gates-and-delivery-conditions) has the full list. When any condition fails, the text parks as a `queued` record for the next qualifying turn boundary. A mux timeout before a durable `sent` record exists parks the text for a durable agent target; `--steer` reports the timeout because it asks for the live pane now. A successful send-now writes the same durable `sent` record as `--steer`.
- `--schedule <DUR|HH:MM>`: always parks and stores a `not_before` timestamp. The room must be open so the sidebar elder can spawn `message sweep` when the wake stamp comes due.

## Addressing and targets

The address grammar (handle classes, channel resolution, arity, fan-out, `--create`) is [harness.md § The address](./harness.md#the-address). This section covers what a target resolves to for delivery: a live pane, or the durable card a parked message keys on.

`message --steer` reaches live panes. A bare `@<kind>` or `@all` also reaches a pane that has not bound a session yet, a lazy-registering agent (Codex) before its first turn ([agent.md § The instance lifecycle](./agent.md#the-instance-lifecycle)), because the thing a paste needs is the pane, which the producer already detects.

The default message path uses that live pane when the target can receive now, including lazy panes with no session yet. When it must park work, it keys the durable record on the bound session or launch placeholder card so FIFO survives registration. A message queued against a provisional `launch_*` card keeps the launch id in the record; when the card registers, name-based matching (`same_card`) folds it into the session's single FIFO queue: one card, one queue.

A petname, kind ordinal, or real session-id prefix names a bound session in every mode; launch placeholder ids stay internal. The `@` sigil is required: a bare selector fails with a `did you mean @…?` hint, so a stray word never broadcasts, and a pane id is the one sigil-free exception. Floating Zellij panes participate in live-pane addressing.

## Send mechanics

### Bracketed-paste submit

Immediate sends wrap the text in bracketed-paste markers (`ESC[200~` to `ESC[201~`) through `MuxBackend::paste_text`, then press Enter as a separate `send_key`. The boundary is lexical. Agent composers run paste-detection heuristics: text plus a trailing `\r` coalesced into one PTY read is taken as pasted content, with the `\r` a literal newline rather than a submit. So the composer leaves paste mode on `ESC[201~`, and the following Enter is unambiguously a keystroke even when every byte arrives in one read.

The discrete writes land one second apart after the first write: paste immediately, wait, submit. This gives a busy composer separate paste and submit events on the PTY. A `\n` inside the text rides the paste as a soft composer newline, so a multi-line prompt lands multi-line. The generic `rimz pane send` stays on the raw type path, since a bare shell would render the markers literally.

### Sender prefix

By default a Rimz-launched agent's send arrives prefixed `from @sender: `, gaining `#channel` when it crosses channels. The recipient lane comes from its registered channel, live pane channel, or addressed channel, so a just-launched same-lane teammate does not gain a spurious suffix before pane capture lands. The handle uses the shortest unique selector: the role when unique in scope, then the profile when unique, else the kind, else the petname. `--no-from` delivers without the sender prefix. The receiver's turn-start hook parses the prefix once and records a first-class `Message` entry in the transcript log with structured `from`; the delivery queue record remains queue bookkeeping, not a transcript source.

A fan-out also prefixes the text with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message.

### Fan-out

A multi-match is an ambiguity error until `--all` or `@all` opts in. Fan-out delivers to every match, prefixes each delivery with the addressed handle, skips a blocked agent while the rest send, and paces deliveries one message interval (1 s default, `RIMZ_MESSAGE_INTERVAL_MS` overrides) apart between pane writes. Broadcasts summarize sent and skipped agents with handles and message ids.

## The message record

Each live message is a record in the workspace message queue, persisted in [the message store](#the-message-store). `msg_` ids are short workspace-unique time-sortable tokens, so id order is FIFO order.

A record stores:

| Field | Purpose |
| --- | --- |
| `message_id` | `msg_` prefixed, time-sortable |
| `workspace_id` | owning workspace |
| `kind`, `agent_id`, `agent_name` | receiver identity; name enables provisional-to-registered FIFO folding |
| `channel` | receiver channel at enqueue time |
| `sender` | `Human` or `Agent { kind, name, profile, role, channel }`, attribution only, never the body |
| `body` | `Prompt` (default) or `Command` (a `/compact` or adapter command) |
| `text` | the message content |
| `enter` | whether to submit with Enter after the paste |
| `gate` | `Done`, `Any`, or hidden `Resume`: the status gate that releases delivery |
| `force` | deliver past a pending ask |
| `pane_id` | pane address when known at enqueue time |
| `status` | lifecycle state (see below) |
| `enqueued_at`, `updated_at`, `delivered_at` | timestamps |
| `attempts`, `last_attempt_at` | pre-send claim retry bookkeeping; `attempts` gates `Abandoned` |
| `unconfirmed_sends` | unconfirmed `Sent` retry count; gates `TimedOut` |
| `last_error` | latest delivery or reconciliation error |
| `not_before` | earliest delivery time for scheduled messages |
| `retry_after` | wake-only retry floor set by the elder sweep; it never gates FIFO readiness |
| `auto_compact` | context-fill threshold that triggers a `/compact` before delivery |
| `compacted_context_tokens` | baseline reading that suppresses duplicate compaction |
| `batch_id` | records sent in one batched paste share the head's id; one turn start confirms them all |

The full record is the field catalog; the [lifecycle](#message-lifecycle) below is the contract. Domain model: [`message.rs`](../../../crates/rimz/src/message.rs); live sends, park-vs-live dispatch, and queued delivery live in [`message/send.rs`](../../../crates/rimz/src/message/send.rs), [`message/dispatch.rs`](../../../crates/rimz/src/message/dispatch.rs), and [`message/deliver.rs`](../../../crates/rimz/src/message/deliver.rs).

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

- `Created` is transient: the record reaches `Queued` before the write returns.
- `Queued` and `Claimed` are open (`is_open`): the message is live in the queue.
- `Sent` means bytes were written to the pane; the record stays live in the queue until confirmation or reconciliation makes a terminal decision.
- `Delivered` means the agent acknowledged the text: `TurnStarted` for a `Prompt`, `Compacting` for a `Command`.
- Terminal states (`Delivered`, `TimedOut`, `Errored`, `Removed`, `Abandoned`, `Archived`) are final; the record is removed from `messages/messages.jsonl` after the terminal event is prepared, and the event log is the transcript for that outcome.

## Gates and delivery conditions

A parked message delivers when all five conditions hold:

1. Gate is open. `DeliveryGate::Done` opens on `Idle` or `Success`; `DeliveryGate::Any` also opens on `Failed`; hidden `DeliveryGate::Resume` opens only on `Paused` after the account-budget resume guard passes. `Running`, `Waiting`, and `Paused` keep ordinary delivery closed.
2. No pending ask. A feed ask attached to the agent's bound session reserves the next input. `--force` bypasses the ask, mirroring `message --steer --force`.
3. FIFO head. The message is the oldest ready queued record for its card and lane. `msg_` id string order is FIFO order; scheduled messages whose `not_before` is still in the future are filtered out, so they never block a later ready message on the same card. Resume nudges use a control lane so a parked-turn wakeup does not wait behind ordinary user text that cannot deliver until after the wakeup.
4. Live pane exists. The target must have a pane that can receive a paste.
5. Hooks are installed and trusted. Parked delivery needs hooks, because hooks are the delivery signal.

`--on done` (the default) and `--on any` set the gate; `--steer` has no gate because it sends immediately.

`DeliveryGate::Resume` is internal. Auto-continue stamps it on the configured resume nudge, and delivery re-checks that the target still reads as a resumable `paused` park with a recovered subscription budget, or an overload park whose marker is still active. Ordinary `Done` and `Any` messages stay parked while an agent is paused.

## Delivery pipeline

### Park path

`queue_message` upserts the record in `messages/messages.jsonl`, appends a `message.queued` audit event, and wakes sidebars. The file is created lazily so an empty workspace costs the hook path one missing-file stat. Each write holds the workspace lock and uses temp-file-plus-rename.

### Delivery trigger

Only unparked root turn ends trigger ordinary parked delivery. `Registered`, subagent stops, compaction events, and parked background turn ends (`TurnEnded { parked_on_background: true }`) do not check the queue. The lifecycle hook records the event, loads pending messages, finds the FIFO head for the agent's card, and spawns a detached `rimz message deliver --message-id <id>` helper with nulled stdio. Auto-continue is the producer-driven exception: when a persisted park reaches its reset or backoff condition, the producer spawns `rimz agents auto-continue`, which queues a `Resume` message or redelivers the prior queued resume message, then runs the same one-message delivery helper for that message id.

### The deliver helper

The helper follows a strict sequence:

1. **Settle**: wait a short delay (400 ms default, `RIMZ_MESSAGE_SETTLE_MS` overrides for tests) for the agent state to stabilize.
2. **Candidate check**: read the queued head, verify `not_before` has passed, the gate is open against a fresh snapshot, the pending-ask predicate holds (skipped under `force`), and a live pane exists.
3. **Claim**: under the workspace lock, transition the record from `Queued` to `Claimed` and increment the pre-send attempt count. The claim moves the record out of the queued scan immediately before sending.
4. **Send**: write text to the live pane through the same bracketed-paste path as `--steer`. Smart compaction prepends a fresh `Command` record at delivery time before the claimed prompt.
5. **Record send**: a successful pane write moves the record to `Sent`, still live until the agent confirms it or the reconciler times it out.

### Batched delivery

When a queued prompt head delivers, the helper extends the claim through the contiguous ready FIFO prefix of that head's lane. Batch members must be prompt bodies that submit with Enter, avoid leading `/`, have their own gate open, match the head's `force` flag, and share one batch key: an agent sender's channel, while a human message counts as the receiver channel as if typed in the pane. `Command` bodies, slash text, no-enter drafts, force mismatches, closed gates, and cross-channel senders stop the batch. Resume control messages live in their own lane and stay outside ordinary batching.

The batch lands as one bracketed paste and one submit. Each member keeps its own `from @sender:` prefix and the sections are separated by one blank line. The first member whose `auto_compact` threshold fires may type one `/compact` command ahead of the whole batch; later members ride the same fresh window.

### Delivery confirmation

The agent's next body-matching lifecycle hook confirms the oldest `Sent` record for that card:

- `TurnStarted` confirms a `Prompt` body to `Delivered`.
- `Compacting` confirms a `Command` body to `Delivered`.

One cannot confirm the other. Batched prompt records carry a shared `batch_id`, so the first matching `TurnStarted` confirms every `Sent` prompt record with that stamp on the same card. A smart-compact send owns two records: the `/compact` command confirms on `Compacting`, and the prompt confirms on `TurnStarted`. A `Sent` record that remains unconfirmed for `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` returns to `Queued` through the sweep reconciler while incrementing `unconfirmed_sends` up to `RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS` (3 by default), then becomes `TimedOut`. The pre-send `attempts` counter stays separate.

### Retry and failure

- **Pre-send failure** (pane gone, gate closed, pending ask blocks): a claim increments `attempts`, then the failure reverts the record to `Queued` with `last_error` and the claim timestamp as throttle. The next qualifying turn boundary retries.
- **Unconfirmed send** (bytes were written but no matching lifecycle confirmation arrives): the sweep reconciler clears the pane id and batch id, increments `unconfirmed_sends`, records `delivery unconfirmed; re-queued`, and retries through the normal FIFO path. A requeued batch member reforms with whatever same-lane prefix is ready at the next boundary. After the unconfirmed-send cap, the record becomes `TimedOut`.
- **Independent caps**: `unconfirmed_sends` gates the unconfirmed-send cap (`RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS`, 3 by default); `attempts` gates the pre-send cap (`MAX_DELIVERY_ATTEMPTS`, 5). A claim increments `attempts` only, and a stale-`Sent` requeue increments `unconfirmed_sends` only.
- **Claim TTL**: a `Claimed` record older than 15 s (`CLAIM_TTL`) is treated as expired, so a crash after claim leaves a redeliverable record. `message list --all` surfaces it.
- A state miss, where the message is queued but the agent has not reached a qualifying boundary, leaves the message queued for a later transition.

### Terminal transitions

| Trigger | Terminal status |
| --- | --- |
| Agent's next lifecycle hook confirms the body | `Delivered` |
| Unconfirmed `Sent` record reaches the unconfirmed-send cap | `TimedOut` |
| Pane write fails after bytes were written | `Errored` |
| User runs `message remove` | `Removed` |
| Retry cap exceeded | `Abandoned` |
| Receiver session `Ended` or channel teardown | `Archived` |

Lifecycle `Ended` archives receiver messages in realtime. Channel teardown archives too: recreating, explicitly removing, or sweeping a worktree channel through cleanup or `rimz gc` moves that channel's open records to `Archived`, and `message list` hides them by default while `message list --all` and `message status <id>` keep the audit trail visible. The message sweep is the primary reconciler for unconfirmed `Sent` records, and `rimz gc` is the durable backstop. `Archived` is distinct from retry exhaustion (`Abandoned`) and explicit user removal (`Removed`).

## Scheduling

`--schedule <DUR|HH:MM>` always parks and stores `not_before`. Durations accept `s`, `m`, `h`, and `d`; wall-clock times resolve to the next occurrence in the configured `timezone` (today if still in the future, otherwise tomorrow), falling back to the system zone when unset. A zero duration is rejected.

A future `not_before` keeps the message out of FIFO scans until it comes due, so a scheduled message never blocks a later ready message on the same card ([Gates and delivery conditions](#gates-and-delivery-conditions)).

Scheduled and parked messages need an open room for wakeups:

1. The CLI writes `message-wake.json` under the runtime root with the earliest future `not_before`, `Queued` retry floor, ready queued backstop, or unconfirmed `Sent` reconcile deadline.
2. The elected sidebar elder reads that cache and, when due, spawns a detached `rimz message sweep`.
3. The sweep helper reconciles stale `Sent` records, finds ready FIFO heads whose gates are open, calls the same one-message delivery path as lifecycle hooks, then rewrites the wake cache to the next future schedule, ready queued retry, or reconcile deadline, or removes it.

Ready `Queued` heads arm the wake stamp as a backstop even when `not_before` is absent or already elapsed. A fresh ready message contributes its `updated_at` timestamp, so the elder sweep recovers an idle-agent message that missed the live send path. When a sweep cannot deliver the FIFO head because the gate is closed, a pending ask reserves input, or the pane is unavailable, it writes `retry_after = now + RIMZ_MESSAGE_DELIVERY_WINDOW_MS`; the elder then retries at most once per delivery window instead of every tick. `retry_after` is a wake hint only: it does not affect `is_ready`, FIFO, claim leases, or the turn-end hook, so lifecycle delivery still runs immediately when the target finishes. Future scheduled messages still arm their `not_before`, and `Sent` records still arm their reconcile deadline.

## Smart compaction

`--smart-compact <PCT|TOKENS>` lands a message against a fresh context window. When the agent's context fill has reached the threshold, Rimz sends a tracked `/compact` command message first, waits one message interval, then sends the prompt message so it runs after compaction instead of racing the agent's own auto-compaction mid-turn.

Threshold forms:

- `70%`: a percentage of the context window; fires when `context_pct >= 70`.
- `120000`: an absolute occupied-token count; fires when occupied tokens >= 120 000.

An omitted flag falls back to the [`[harness] smart_compact`](../../reference/configuration.md#smart-compaction) default. An unknown fill never triggers: a missing reading is not a full window, so it sends untouched.

Reading sources, in order: the folded statusline reading where present (`context_pct`), else the per-call token split (`cache_read_input_tokens + fresh_input_tokens`), else the carried `total_tokens` gauge.

Compact-first path: the `/compact` command rides the raw type path, not the bracketed paste, because a composer treats pasted text as literal content, so a pasted `/compact` would land as a prompt rather than run. The compact-first path paces `/compact`, its submit, the message, and its submit one second apart after the first write so compaction settles before the message arrives.

Baseline tracking: `compacted_context_tokens` records the token reading the trigger fired on. While a carried-forward stale gauge still equals this baseline, the send path suppresses duplicate `/compact` commands; a new reading re-enables compaction.

Parked records store the threshold in `auto_compact` and re-read fill at the delivery boundary, typing `/compact` ahead of the message in the same delivery so a failed compaction fails the delivery through the same retry path as a failed send.

## Wait

`--wait[=DURATION]` upgrades `message --steer` and send-now default messages from fire-and-return to synchronous confirmation. The command waits until the prompt record reaches `Delivered`, `TimedOut`, `Errored`, `Removed`, `Abandoned`, or `Archived`, prints the matching terminal status with handle and message id, and exits nonzero unless delivered. Bare `--wait` uses `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` or the default delivery window (30 s). It conflicts with `--no-enter`, because an unsubmitted paste cannot be confirmed.

Broadcast waits share one deadline across all prompt records.

Smart-compact waits track two records: the `/compact` command confirms on `Compacting`, and the prompt confirms on `TurnStarted`; one cannot confirm the other.

Edge cases:

- `--force` sent mid-turn can time out because a resumed turn emits no fresh `TurnStarted` for that paste.
- A sessionless lazy pane confirms only after a real session or name can match its pane-derived placeholder record, so the first prompt can time out even when the paste succeeds.

## Channels

Every target lives in a channel. A channel is a cooperation lane inside one room: the identity the sidebar groups by, the suffix an address uses as `#channel`, and the tab name Rimz recovers on rebirth.

### Backings and labels

Four backings can produce a channel:

- **Named channel**: a durable bare name created by `rimz channel new design` or first use through `--channel design`; it carries `RIMZ_CHANNEL=design`.
- **Worktree channel**: a Rimz-owned Git worktree; the branch is the preferred label and the worktree name and path stay addressable aliases.
- **Team channel**: an in-place named team under one directory, labelled `<dir>/<team>` and carried by `RIMZ_TEAM`.
- **Directory channel**: the directory basename used when a live agent has no named, worktree, or team identity.

Label precedence is explicit named channel, then worktree branch, then `<dir>/<team>`, then directory basename. This single rule feeds target resolution, rendered handles, sidebar grouping, `agents list`, pane overlays, and recovery.

Sidebar pods keep identity and kind separate: a named-channel pod stores `label = design` and renders the channel hash glyph plus that bare name; a worktree pod stores the branch label and renders the branch or merge glyph; a non-repo room root stores the directory basename and renders no glyph.

Git isolation follows the agent's own resolved worktree, not the room tree. Hooks run `git rev-parse --show-toplevel` from the agent cwd at any depth, and a git-backed row contributes that toplevel as its grouping root. Directory rooms do not scan child repos; non-git agents at the room root or in non-git subdirs fold into the room's root pod, while a nested checkout that an agent is actually working in earns its own worktree pod.

### The named-channel registry

Named channels live in `channels.json` beside `workspace.json` in the workspace ledger. The record stores the bare name and creation time; writes hold the workspace lock and use temp-file-plus-rename.

The registry stores only named channels. Worktree channels use their `rimz-worktree.json` marker as durable truth, while team and directory channels derive from live launch identity. `rimz channel list` unions the registry, Rimz-owned worktrees, and live channels from the snapshot.

The sidebar stays presence-driven: a group appears when a pane is running in that channel. An empty named channel persists in `channels.json`, appears in `rimz channel list`, and reopens as an empty `#channel` tab on room rebirth. Named-channel records stay until `rimz channel rm`; `rimz gc` acts on worktrees only.

Named channels and Rimz-owned worktrees reserve the same bare channel namespace. `rimz channel new NAME` refuses an existing worktree channel, and `rimz worktree new NAME` refuses an existing named channel with the fix to use the named-channel command or choose another name.

### Launch and address

`rimz agents <SPEC> --channel design` registers the channel if needed, stamps `RIMZ_CHANNEL`, opens a `#design` tab, and writes the channel into the launch event so the rollup survives hook timing and recovery. `rimz message --steer @planner#design --create "draft"` follows the same path for create-on-miss.

`--worktree` and `--channel` are separate launch intents. A worktree launch creates or reuses Git backing; a named-channel launch stays in the room root and records only the bare lane. Inline `#design` and `--channel design` reconcile through the same target parser, so mismatched channel names fail before delivery.

Commands run inside a named-channel tab inherit `RIMZ_CHANNEL`, so `@claude` scopes to that lane by default. Human shells in a bare directory room have no current channel and reach the whole room unless an address or flag supplies one.

## The message store

The ledger persists live message state in one JSONL file under the workspace state root, with terminal history in the shared event log:

```text
messages/messages.jsonl   live queued, claimed, and sent records
events.log.jsonl          terminal message.* audit events
```

`messages/messages.jsonl` holds only live records. A terminal transition removes the record from the queue file, then appends the terminal `message.*` event; a crash in between cannot redeliver the message, and the cost is at most a missing terminal audit row. All writes use temp-file-plus-rename through the ledger atomic helpers and hold the workspace lock.

The store exposes `list()` (live records) and `list_pending()` (`Queued` records only). On first access, a legacy `messages/<msg_id>.json` plus `messages/terminal/` layout migrates live `Queued`, `Claimed`, and `Sent` records into the JSONL file and discards terminal files already represented by the event log. Store implementation: [`ledger/message_store.rs`](../../../crates/rimz/src/ledger/message_store.rs); ledger mutations: [`ledger/writer/queue.rs`](../../../crates/rimz/src/ledger/writer/queue.rs).

## Transcript

Routing text to an agent is the write side; the transcript log is the durable record of the resulting conversation, and `rimz transcript` reads it back as a chat timeline. The log is Rimz-owned, distinct from a provider's native session files, so ended agents, past channels, and their asks and answers stay visible after those native files rotate away or leave the live snapshot.

Hook and resolver paths append entries to `transcript/<bucket-start>.jsonl`, append-only and under the workspace lock. `[transcript] file_days` sets the bucket width for file-size control ([configuration.md](../../reference/configuration.md)); buckets are never pruned, and reads sort by recorded timestamp, so a bucket boundary carries no ordering meaning. Each entry stores a kind, the receiving agent's identity and channel, a timestamp, the text, and the structured `from`, `questions`, or `answers` its kind needs. Six kinds cover the conversation surface:

| Kind | Records | Reads back as |
| --- | --- | --- |
| `Prompt` | a human prompt to an agent | `user: @receiver, text` |
| `Message` | an inter-agent delivery, tagged with structured `from` | `@sender: @receiver, text` |
| `Assistant` | a root turn's final assistant message | `@receiver: text` |
| `Ask` | a native question ask; its `questions` carry option labels and descriptions | the agent's question |
| `Answer` | the effective answer, carrying `answers` choices | `you` or the resolver to the agent |
| `Error` | a hook-path provider error marker newly merged into `AgentContext.turn_error` | `@receiver: error text` with error styling |

A delivery becomes a `Message` entry when the receiver's turn-start hook parses the `from @sender` prefix ([Sender prefix](#sender-prefix)); the delivery queue record stays bookkeeping, never a transcript source. A batched delivery splits on the blank-line boundaries that introduce another sender prefix, so each section becomes its own transcript entry. A peer-opened turn also records the receiver's reply, because that reply is its own `Assistant` entry. A provider turn-error becomes an `Error` entry only on the hook-path merge (`StopFailure` or a `Stop` tail refresh). Statusline-only detections stay card/sidebar enrichment, because that path is lock-free and does not write the transcript log.

`rimz transcript` projects these entries into one timestamp-ordered chat log. A channel target (`#channel`, `@all#channel`, or a bare invocation in a worktree) shows every agent in the lane; a single-agent target filters to that agent's sent and received lines. Exact session ids resolve across channels; handle targets prefer live sessions in the current room, then the most recent transcript activity. The command surface, flags, and rendered appearance are [cli/agents.md → Inspect transcripts](../../reference/cli/agents.md#inspect-transcripts).

Two nearby reads are not this log. Supervised-run streaming (`agents wait --stream`, `--output-format stream-json`) tails the provider-native transcript through each adapter's `parse_transcript_messages` ([harness.md → Supervised runs](./harness.md#supervised-runs)), and the context-fill and spend gauges read those same native files ([agent.md → Enrichment](./agent.md#enrichment)). The audit trail below is a third log: operational `message.*` events that carry no message content.

Domain types: [`ledger/transcript_log.rs`](../../../crates/rimz/src/ledger/transcript_log.rs) for the durable log, [`agents/transcript.rs`](../../../crates/rimz/src/agents/transcript.rs) for the chat projection.

## Audit trail

Every status transition appends a typed event to `events.log.jsonl`. The event methods are:

`message.queued` · `message.sent` · `message.delivered` · `message.timed_out` · `message.errored` · `message.removed` · `message.abandoned` · `message.archived`

The payload carries `message_id`, `kind`, `agent_id`, `agent_name`, `channel`, `gate`, `status`, `body` (Prompt or Command), `pane_id` (when known), `forced` flag, `sender` attribution, `text_len`, `attempts`, `unconfirmed_sends`, timestamps, compaction baseline, and `reason` (on error or abandon). Message content stays in the live message record, never in the event.

## Subcommands

User-facing:

- `message list`: defaults to the current channel, hides archived records, sorts newest first, renders `ID STATUS TARGET FROM CREATED DELIVERED TEXT`. Live rows come from `messages/messages.jsonl` and include text; terminal rows come from the event log and omit text. `--all` widens the view, `--channel <NAME>` selects one channel, `--status <STATUS>` filters exactly, `--json` emits the merged projection including attempts and unconfirmed sends.
- `message status <msg_id>`: prints one live record or terminal projection as key/value detail: status, target, sender, channel, timestamps, attempts, unconfirmed sends, last error, and text preview when the live record still exists.
- `message remove <msg_id>`: removes a queued or claimed message → `Removed`.
- `message clear <target>`: removes every open message for one agent card. Accepts `--worktree` and `--channel` for scoping.

Hidden helpers, spawned by hooks and the sidebar elder, not for human use:

- `message deliver --message-id <id>`: the detached delivery subprocess spawned by lifecycle hooks at unparked root turn ends.
- `message sweep`: the detached helper spawned by the sidebar elder when a message wake stamp comes due; finds ready FIFO heads, delivers, backs off blocked heads, rewrites the wake cache.

## Hazards

- Queued text can land while a human has half-typed a draft in the agent pane. Rimz gates on ledger state, not focused-pane state or captured composer contents.
- Agent UIs can present dialogs that are not feed asks. Core keeps pane capture out of delivery; a resolver that needs to inspect UI text owns capture-before-send.
- Multiplexer sends are best-effort: a pane can disappear or reject input after the claim, which the message record records and retries until the attempt cap.
