# The message system

> How text reaches a running agent: the durable record, the delivery decision, the pane write, the compaction commands and cache pings that ride the same path, the reply wait, and the channel lanes that scope addressing. The code is `crates/rimz/src/message/`. [fleet.md](./fleet.md) maps this area and owns the [address grammar](./fleet.md#the-address) this module resolves through. [transcript.md](./transcript.md) owns the conversation log that confirmed deliveries write and the ask records that hold delivery back. For users, the command is [cli/message.md](../../reference/cli/message.md).

## What the module does

RimZ gives an agent work by typing into its pane. Agents run their stock CLIs in real terminal panes with no API into them, so `rimz message` types on behalf of a human, a script, a loop task, or another agent.

The receiver is usually busy. It can be mid-turn, blocked on a permission prompt, compacting its context, or not yet launched. Typing at the wrong moment either redirects work in flight or leaves text in a composer nobody submits. One rule handles every case:

> **The durable record is the message. The pane write is only an attempt.**

Every send persists a `MessageRecord` before a byte reaches a pane. If the receiver can take the text now, the same call writes it and marks the record `Sent`. If it cannot, the record stays `Queued` and the agent's next turn boundary delivers it, oldest first. A busy agent, a closed and reopened room, a failed multiplexer write, or a crash between claim and send loses nothing, because the text lives in the record.

The rest of the page follows from that rule: how a record decides it is ready, who wakes up to deliver it, how a write is confirmed, and what happens when confirmation never arrives.

## Module layout

| File | Owns |
| --- | --- |
| [`message.rs`](../../../crates/rimz/src/message.rs) | Delivery assembly and parsing: per-recipient record construction, delivery gates, schedule parsing, the timing knobs, and `spawn_queued_head`. No I/O. |
| [`message/dispatch.rs`](../../../crates/rimz/src/message/dispatch.rs) | One send request end to end: resolve targets, bind conditions, decide park or live, preflight hooks, enqueue, and pace the fan-out. |
| [`message/deliver.rs`](../../../crates/rimz/src/message/deliver.rs) | Readiness: the ordered delivery check, the delivery attempt and its failure recovery, condition evaluation, and the sweep with its wake rearm. |
| [`message/send.rs`](../../../crates/rimz/src/message/send.rs) | The pane write: bracketed paste, typed commands, the interrupt key, pacing, the Sent-before-submit barrier, and the compact-first command. |
| [`message/synthetic.rs`](../../../crates/rimz/src/message/synthetic.rs) | RimZ-authored text for a card (`SyntheticMessage`, `deliver_now`, `attempt_now`): the record's channel, sender, and pane, the boundary attempt, and the recorded miss. |
| [`message/compact.rs`](../../../crates/rimz/src/message/compact.rs) | Standalone compaction for the operator verb, idle compaction, and flip compaction: repeat refusal and boundary delivery. |
| [`message/reply.rs`](../../../crates/rimz/src/message/reply.rs) | `--wait`: leg state machines, transcript anchoring, cycle detection, join settlement. |
| [`message/fire.rs`](../../../crates/rimz/src/message/fire.rs) | The host's side of the clock: read the wake stamp and spawn `message sweep`, naming the room by workspace id and mux. |
| [`store/message.rs`](../../../crates/rimz/src/store/message.rs) | The record schema and vocabulary, card matching, FIFO, claim, and batch selection, prompt alignment for [confirmation](#confirmation-and-retry), header parsing, and the read-only submitted-prompt origin classifier. |
| [`store/message/codec.rs`](../../../crates/rimz/src/store/message/codec.rs) | The JSONL codec for the live queue and terminal history. |
| [`store/message/sender_notice.rs`](../../../crates/rimz/src/store/message/sender_notice.rs) | [Sender notice](#sender-notices) text and live-sender resolution. |
| [`store/writer/queue.rs`](../../../crates/rimz/src/store/writer/queue.rs) | Every status transition, under the workspace lock, with its audit event where one is written. Every live-queue write refreshes the host wake stamp before releasing the lock. |
| [`cli/message/`](../../../crates/rimz/src/cli/message) | Flag parsing, rendering, reply polling, and the inbox verbs. No delivery logic. |
| [`cli/hooks/lifecycle/delivery.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/delivery.rs) | The hook side: confirm sent records and spawn the delivery helper at turn boundaries. |

Three neighbours carry pieces this module depends on. [`address.rs`](../../../crates/rimz/src/address.rs) resolves `@handle#channel` addresses, binds a match to a pane, and renders handles. [`transcript.rs`](../../../crates/rimz/src/transcript.rs) is the conversation log. [`channel.rs`](../../../crates/rimz/src/channel.rs) owns the lane-name rules.

The layering runs one way: `dispatch` calls `deliver` and `send`, `deliver` calls `send`, and `send` calls the store and the mux. Nothing calls back up.

## The record

A record is keyed on a card, the logical agent identity the [rollup](../agents/model.md#the-rollup) tracks: a kind plus a session id, with the stable `agent_name` as a second key. The name lets a message address an agent before it registers a session. A record queued against a provisional `launch_*` id keeps that id, and `same_card` folds it into the real session's queue once registration lands. Each card has one FIFO queue, with `Resume` control messages in a separate lane.

`msg_` ids are workspace-unique and time-sortable, so string order is FIFO order. A fresh boundary send assigns its id inside its claim commit, after every record live on the card, which keeps that order across concurrent senders. The module relies on this everywhere: replacing the id scheme means replacing the ordering with it.

| Field | Purpose |
| --- | --- |
| `message_id` | `msg_` prefixed, time-sortable, FIFO by string order |
| `workspace_id` | Owning workspace |
| `kind`, `agent_id`, `agent_name` | Receiver card. The name folds provisional ids into the registered queue |
| `address` | Receiver handle as resolved at enqueue, rendered by `list` and `show` after the live card is gone |
| `channel` | Receiver lane at enqueue |
| `sender` | `Human`, `Agent { kind, agent_id, name, profile, role, channel }`, `Harness { notice }` (rendered as `@rimz`), or `System` (rendered as `rimz`). `Subagent { kind, name }` decodes but nothing writes it |
| `automated` | Background orchestration traffic. Never earns a dollar-budget waiver, and makes a queued command perishable |
| `reply_wait` | A CLI is blocked on this record's reply ([Reply waits](#reply-waits)) |
| `in_reply_to` | The messages that opened the sender's authoring turn. Empty starts a new conversation ([transcript.md § Causality](./transcript.md#causality)) |
| `about_run` | Optional run id. Delivery withdraws the record when that run is terminal or gone; omitted when absent and preserved by requeue |
| `body` | `Prompt` (pasted) or `Command` (typed, such as `/compact`) |
| `text` | The content |
| `enter` | Submit with Enter after the write |
| `gate` | `Done`, `Any`, or internal `Resume`: which agent statuses release delivery |
| `force` | Deliver past an agent that is holding a question |
| `pane_id` | Pane pin, set when a pane resolved at enqueue. Only a requeue after a failed attempt clears it |
| `status` | [Lifecycle state](#status-lifecycle) |
| `not_before` | Scheduled delivery floor |
| `register_by` | Registration deadline for a child's resume, shared by its waiting follow-ups. Inert outside `Queued` |
| `after`, `when` | Cross-agent conditions, each with a durable `met_at` stamp |
| `auto_compact` | Context-fill threshold that fires a `/compact` ahead of the text |
| `compacted_context_tokens` | The context reading a compaction fired on, so a stale gauge cannot fire it twice |
| `batch_id` | Shared by records written in one paste. A turn start without prompt text confirms the batch together |
| `retry_after` | Wake hint set by the sweep. On a `Sent` record held for compaction, ingress, or provider start, the shared reconciliation and read-side deadline |
| `attempts`, `last_attempt_at` | Claim bookkeeping. The claim stamp is the hold a sender must still match |
| `last_sent_at` | Last pane write. Survives a prompt requeue so a late acknowledgement can still settle it |
| `unconfirmed_sends` | Prompt requeues after a write no hook confirmed |
| `queued_notice_at` | When the [still-queued notice](#sender-notices) went to the sender. Never cleared on the same message |
| `last_error` | The latest delivery or reconciliation failure, a refused synthetic attempt's verdict, or a `NoPane` blocker |
| `enqueued_at`, `updated_at`, `delivered_at` | Timestamps |

`MessageSender::is_conversation()` splits senders by variant alone. `Human` and `Agent` are conversation traffic. `Harness`, `Subagent`, and `System` are system traffic, which covers nudges, compaction commands, and deliberately unattributed `--no-from` text. The split is a read-side filter for `message list` and other conversation surfaces, and `automated` plays no part in it.

Two counters track two different failures, and they stay separate. `attempts` counts claims: a claim bumps it, and a retryable failure with `attempts` at `MAX_DELIVERY_ATTEMPTS` (5) ends the record `Abandoned`. `unconfirmed_sends` counts requeues of a prompt whose write no hook confirmed. Reconciliation requeues while it is below `DEFAULT_MAX_DELIVERY_ATTEMPTS` (3, `RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS` overrides it) and times out the next stale write, so a prompt reaches the pane at most four times by default. Commands have no requeue at all, because they are never resent.

### Turn-opening injection sites

These are the producers of queued or launch-time text. Paths are relative to `crates/rimz/src/`. Sender and body names are `MessageSender` and `MessageBody` variants. A launch prompt is not a queued `Prompt` record.

| Site | Module and composing symbol | Sender / body queued |
| --- | --- | --- |
| Human or agent message | `crates/rimz/src/cli/message/dispatch.rs::send_message`, caller text. `crates/rimz/src/launcher/context.rs::sender_for` resolves identity | `Human` or `Agent` / `Prompt`. `--no-from` uses `System` / `Prompt` |
| Fleet digest | `crates/rimz/src/cli/agents_cmd/subagent_report.rs::report_fleet_with_kind`, text from `compose_digest` | `Harness { notice: SubagentReport }` when every row is a subagent run, else `AgentReport` / `Prompt` |
| Team report | `crates/rimz/src/cli/agents_cmd/team_report.rs::report_team` | `Harness { notice: TeamReport }` / `Prompt` |
| Child parked on a provider limit | `crates/rimz/src/cli/agents_cmd/park_notice.rs::run`, text from `crates/rimz/src/harness/park_notice.rs::text`, through `SyntheticMessage` | `Harness { notice: SubagentPaused }` / `Prompt` |
| Launched child silent mid-turn or never started | `crates/rimz/src/cli/agents_cmd/stall_notice.rs::run`, text from `crates/rimz/src/harness/stall_notice.rs::text`, through `SyntheticMessage` | `Harness { notice: SubagentStalled }` / `Prompt` |
| Agent-sent message ended undelivered | `crates/rimz/src/store/writer/queue.rs::QueueTxn::close` collects endings, `Store::commit_queue` composes through `crates/rimz/src/store/message/sender_notice.rs::undelivered` | `Harness { notice: MessageUndelivered }` / `Prompt` |
| Agent-sent message still queued | `crates/rimz/src/message/deliver.rs::sweep`, text from `crates/rimz/src/store/message/sender_notice.rs::still_queued` | `Harness { notice: MessageQueued }` / `Prompt` |
| Prompt-cache keepalive | `crates/rimz/src/cli/agents_cmd/cache_keepalive.rs::run`, text from `crates/rimz/src/harness/cache_keepalive.rs::prompt`, through `deliver_now` | `Harness { notice: CacheKeepalive }` / `Prompt` |
| Auto-continue | `crates/rimz/src/cli/agents_cmd/auto_continue.rs::run_auto_continue`, configured resume text through `SyntheticMessage` and `deliver_now` (or `attempt_now` for a queued id) | `System` / `Prompt` |
| Supervised verification reprompt | `crates/rimz/src/harness/prompt_compose.rs::verify_reprompt`, delivered by `crates/rimz/src/cli/supervised/verify.rs::deliver_reprompt` through `deliver_now` | `System` / `Prompt` |
| Fleet or account budget continuation | `crates/rimz/src/cli/budget.rs::run`, configured resume text through `SyntheticMessage` | `System` / `Prompt` |
| Agent budget continuation | `crates/rimz/src/cli/agents_cmd/budget.rs::run_budget`, configured resume text through `SyntheticMessage` | `System` / `Prompt` |
| Idle compaction | `crates/rimz/src/cli/agents_cmd/idle_compact.rs::run_idle_compact`, `agents::compact_command`, `crates/rimz/src/message/compact.rs::send_compact` | `System` / `Command` |
| Stage-flip compaction | `crates/rimz/src/harness/team_stage.rs::compact_flipper`, `agents::compact_command`, `crates/rimz/src/message/compact.rs::send_compact` | `System` / `Command` |
| Operator compaction | `crates/rimz/src/cli/agents_cmd/compact.rs::compact_agent`, adapter compact command, `crates/rimz/src/message/compact.rs::send_compact` | Operator's `Human` or `Agent` / `Command` |
| Smart compact before delivery | `crates/rimz/src/message/send.rs::compact_message_for_target`, `agents::compact_command` | `System` / `Command` |
| Stage notice | `crates/rimz/src/harness/team_stage.rs::stage_open_body`, queued by `open_stage` from a flip, the exec wrapper's resume (`rewake_resumed`), or the registration backstop ([teams.md § Resume re-wake](./teams.md#resume-re-wake)) | `Harness { notice: Stage }` / `Prompt` |
| Loop delivery, self-wait, or watch delivery | `crates/rimz/src/harness/schedule/runner.rs::resolve_effect_prompt`, `crates/rimz/src/harness/schedule/runner/prompt.rs::compose_wait`, dispatched by `crates/rimz/src/cli/loop_cmd/run.rs::execute_prepared_delivery` | `Harness { notice: Wait }` / `Prompt` |
| Signal delivery | Same composition and dispatch, for a `Trigger::Signal` row. Team bindings are armed by `crates/rimz/src/harness/schedule/team.rs::arm_member` | `Harness { notice: Signal }` / `Prompt` |
| Loop-spawned run | `crates/rimz/src/harness/schedule/runner.rs::compile_spawn_request` and `resolve_effect_prompt` | No queue record. `RunRecord.loop_task` supplies harness origin |
| Supervised launch or subagent brief | `crates/rimz/src/launcher/resolve.rs::supervised_prompt`, fallback reminder from `crates/rimz/src/harness/launch_reminders.rs::subagent_reminder` | No queue record. Run origin is human or the subagent's parent |
| Supervised retry | `crates/rimz/src/harness/prompt_compose.rs::retry_prompt` | No queue record. Keeps the run's origin |
| `message --create` on a missing recipient | `crates/rimz/src/cli/message/dispatch.rs::recipient_miss`, caller text passed as a launch prompt | No queue record. Agent-sender preservation is deferred |

Run-origin matching and the wrapper registry are in [transcript.md § Writing entries](./transcript.md#writing-entries). Text outside this table has no message record. Direct typing and `rimz pane send` are raw keystrokes whose caller is the author. Ordinary root launch text is not queued. Team system prompts use the system-text channel, and restart, fork, resume, and rebirth inject no turn-opening text. Provider control payloads pass through `SanitizedPrompt`, process plugins included, before they can become lifecycle prompts.

## Status lifecycle

```text
Queued ──► Claimed ──► Sent ──► Delivered
   ▲          │          │
   │          │          ├──► Queued     (unconfirmed prompt, below the requeue cap)
   │          │          └──► TimedOut   (unconfirmed command, or prompt at the cap)
   │          ├──► Queued     (retryable pre-send failure, expired claim)
   │          ├──► Abandoned  (retryable failure at the claim cap)
   │          └──► Errored    (repeat compaction, or a send failure with no retry path)
   │
   ├──► Delivered  (late acknowledgement of an earlier unconfirmed write)
   ├──► Canceled   (user, or a joined digest)
   ├──► Expired    (automatic command's validity window, or absent child's registration deadline)
   ├──► Archived   (receiver or watched session ended, channel torn down)
   └──► Errored    (no receiver, or a send failure with no retry path)
```

`Queued` and `Claimed` are the open states (`MessageStatus::is_open`). `Sent` is live but not open: it stays in the live queue until a hook confirms it or reconciliation gives up, because a write does not prove the agent took the text. A `Sent` prompt also holds its card's boundary until it is acknowledged or its reconcile deadline passes ([the FIFO hold](#the-fifo-hold)).

`Delivered` means the agent acknowledged: `TurnStarted` for a `Prompt`, `Compacting` for a `Command`. Neither event confirms the other body.

### Claims and holds

`Claimed` is a short lease taken immediately before a write, so a concurrent deliverer cannot double-send. Every attempt holds one, a fresh send and a compact-first command included. A claim older than `CLAIM_TTL` (15 s) counts as expired, so a crash mid-send leaves a record that the next sweep requeues and redelivers through the normal FIFO path. The claim's queue commit refreshes the recovery wake under the workspace lock. A failed refresh warns and the send continues, because a wakeup improves recovery latency and is never delivery truth.

The rules that keep two writers from settling one record:

1. A fresh boundary send checks its card's lane in the same commit that enqueues and claims it. A deliverable `Queued` non-`Resume` record, an unexpired non-`Resume` claim, or an unacknowledged non-`Resume` `Sent` prompt inside its hold deadline parks the new record `Queued` behind that blocker. Dispatch's earlier queue read is only a fast path.
2. Under the pane lock, before pacing or writing any byte, `write_batch` checks that every batch member is still claimed (`Store::message_claims_held`). If any claim was lost, it writes nothing, releases the remaining claims, and stops without recording a failure. A compact-first command is the exception: [smart compaction](#smart-compaction) writes its command before the prompt batch's own check runs.
3. A retry, error, or release names the record the sender was handed. The writer applies it only while the live record is `Claimed` with the same `attempts` and `last_attempt_at`. A retry clears `last_attempt_at`, so the record is claimable at once and held by nobody.
4. A sender that was never handed a claim (the boundary helper in `synthetic.rs`) can settle only a `Queued` record, and leaves a `Claimed` one alone.
5. Recording `Sent` needs no hold, because the text is already in the pane. It marks only a live record: one that a cancel, clear, or archive already finalized stays terminal.
6. A give-up on a record that is no longer live writes nothing, so the record keeps its single terminal history entry and event.

### Terminal states

The seven terminal states are final. A terminal transition removes the record from `records/messages/messages.jsonl`, appends the full record to `audit/messages/<bucket-start>.jsonl`, and appends a `message.*` audit event without text ([Storage and audit](#storage-and-audit)).

| Trigger | Terminal status |
| --- | --- |
| A lifecycle hook correlates with the submitted record | `Delivered` |
| A queued automatic command reaches its validity deadline | `Expired` |
| A queued resume follow-up reaches `register_by` with its card absent or ended no later than enqueue | `Expired` |
| An unconfirmed command reaches its delivery deadline | `TimedOut` |
| An unconfirmed prompt goes stale again after its last permitted requeue | `TimedOut` |
| The address resolved to no agent, after the durable fallback | `Errored` |
| A send failure with no retry path: a live pane target with no durable card, a failed compact-first command write, or a fresh steer to an agent waiting on input (a self wait's wake stays `Queued` instead) | `Errored` |
| A queued compaction command would follow an unprompted compaction ([the repeat guards](#the-repeat-guards)) | `Errored` |
| `message cancel`, `message clear`, the [join guard](#the-subagent-digest-join-guard), or the [run-link guard](#the-run-linked-notice-guard) | `Canceled` |
| A retryable failure with `attempts` at its cap | `Abandoned` |
| The receiver session ended, a watched `when` session ended, or the channel was torn down | `Archived` |

`Archived` records a conversation that no longer exists. `message list` hides archived records, while `--all` and `message show` keep them readable. Lifecycle `Ended` archives in realtime, and `rimz gc` is the durable backstop.

## Sending

### Caller identity

The caller resolver decides whether an invocation is agent-authored before dispatch builds its `MessageSender`. It first reads the stable RimZ launch identity from the inherited environment. Without one, it walks the invoking process's ancestry and matches each ancestor's PID and process-start token against a live durable agent `RuntimeOwner`. The nearest match supplies that agent's durable card, even for a provider RimZ did not launch, and prefers the current pane when several rows share the owner. No match means a human sender.

The agent sender's optional `agent_id` uses the same identity as `LaunchedBy.agent_id`: the caller's launch id, or its session id for a row with no launch id. An environment caller with no launch id resolves its unambiguous pane before stamping it. A record without the field decodes and omits it on rewrite, and a name never supplies a missing identity. The field does not change the rendered header.

Daemon-routed sessions, such as a user-run `codex` or a phone-started background session, stay unattributed on the ancestry path. Their tool commands run below the shared app-server, whose `RuntimeOwner` cannot say which agent invoked the command. RimZ-managed Codex launches run embedded through `--no-daemon`, with pane-owned hooks ([Codex registration](../agents/adapter_codex.md#session-registration-and-launch-quirks)).

### Four modes on one timing axis

Every mode resolves targets through the same parser, writes the same record shape, uses the same pane write, and emits the same audit events. The modes differ only in when the record may deliver.

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Boundary | default | Write now if the receiver can take it, otherwise park for the next qualifying turn boundary. |
| Steer | `--steer` | Write into the live turn now, after any in-flight pane write. Conflicts with `--schedule` and `--on`. |
| Interrupt | `--interrupt` | Write as steer does, then press the adapter's interrupt key once ([the interrupt key](#the-interrupt-key)). Claude and Codex declare Escape, and other kinds are refused. Conflicts with every other timing mode and with `--after` and `--when`. |
| Schedule | `--schedule <DUR\|HH:MM>` | Always park, with a `not_before` floor. |

Steer still writes a `Queued` record first and moves it to `Sent` when the paste lands. When the address resolves only to a durable card with no live pane, steer parks, prints `queued for @handle (msg_...)`, and the retry path delivers once a pane appears.

Internal callers pick a mode too:

- Self-only timer and watched-command waits steer. When a native prompt is open at the pane-write check, the `HarnessNotice::Wait` record stays `Queued` with gate `Any` and delivers at the turn boundary after the prompt is answered. Check-ins behave the same way.
- Human steers and deadline notices refuse an agent waiting on input with the waiting-input error.
- Scheduled loop and signal deliveries, team bindings included, use boundary with the `Done` gate.
- `rimz message @me` resolves the calling agent to its pinned session through the shared CLI resolver, and refuses an unidentified, provisional, or ended caller.

### Conditions

Two flags add cross-agent conditions to boundary and schedule sends, and they compose:

- `--after <ADDR>` holds until that agent finishes its queued work. Each address must resolve to exactly one durable card, and repeats form an all-of set. The recipient itself and fan-out addresses are rejected.
- `--when '@handle <status> <duration>'` holds until one agent stays continuously in a raw lifecycle status for the dwell. Self-reference is allowed, which is what makes keep-warm messages (`@codex --when '@codex idle 58m'`) work.

Delivery gates and `when` conditions read status differently, on purpose. Gates read `effective_status()`, which projects budget parks to `Paused`, settles hookless turns, and reads a clean turn parked on background work as `Success`. `when` conditions read the stored `status` field, so a projection never trips a dwell the event log cannot justify.

A condition is met for delivery only once its `met_at` stamp is written (`MessageRecord::conditions_met`). Dispatch stamps a condition already satisfied at send time, and the sweep stamps the rest ([Scheduling and wakeups](#scheduling-and-wakeups)). Upstream work must therefore be queued before the message that waits on it.

### The dispatch walk

[`dispatch()`](../../../crates/rimz/src/message/dispatch.rs) is the one entry point for an owned send.

1. **Load what the decision needs.** Pending records (boundary mode or an explicit resuming recipient), plus agent-context sidecars when a smart-compact threshold, a condition, an agent sender, or `--wait` needs them.
2. **Choose a snapshot.** A full resolution snapshot costs a multiplexer round trip. When every target parks without a live pane (no lazy-registering kind, no provisional id), `targets_all_park_without_live` proves the cached rollup is enough and dispatch skips the mux. This `rollup_only` path is a latency optimization, and correctness never depends on it.
3. **Resolve targets.** Live agents and live panes combine, so an agent with a bound pane yields one target. When the live view finds nothing, the durable audit rollup is the fallback, with pane-shadowed co-resident sessions filtered out first. For intrinsic `@all`, dispatch removes an agent caller from the resolved set before anything else runs, and no remaining peer is an error.
4. **Bind conditions.** Each `after` and `when` address resolves once and pins a card. A condition already satisfied gets `met_at` at once.
5. **Decide park or live, per target** ([below](#park-or-live)).
6. **Deliver, per target in order.** A parked target first passes the hook preflight. Every target then gets its durable record. Live targets go straight into a delivery attempt, and parked ones stop at `Queued`.

A multi-match without `--all` or `@all` is an ambiguity error listing candidates by paste-ready address with status and activity age. Durable candidates use their session ids when available, under a `recorded` header instead of `live`.

The hook preflight in step 6 exists because turn-end hooks are what release parked text. A parked record for a kind without installed, trusted hooks would never deliver, so dispatch refuses it. The preflight runs once per login key (kind plus login account) inside the per-target loop, so in a fan-out a later target's failure returns an error after earlier targets were already queued or sent.

Each queue write in the walk refreshes the wake stamp inside the store commit, so the host knows when to look again without a caller-side step. A stamp that cannot be written warns and never changes the send's reported outcome.

#### Park or live

`dispatch_decision` parks an explicit resuming recipient in every send mode: steer and interrupt become its next boundary prompt. For any other target it takes the first rule that applies.

1. Steer and interrupt go live when a pane is bound and park otherwise. Interrupt preflights every target before enqueue, and always loads agent-context sidecars for that check. A missing key, a live pane without a durable bound session, or an awaiting-input row without `--force` refuses the whole send, fan-out included.
2. A schedule or an unmet condition parks.
3. A receiver that cannot take a prompt parks with a reason: its effective status, or its native-input wait. Readiness comes from the exact pane binding when there is one, and from the durable card otherwise.
4. No resolved pane parks.
5. A deliverable queued record, an unexpired non-`Resume` claim, or an unacknowledged non-`Resume` `Sent` prompt for the same card parks, so a fresh send never jumps the queue or an in-flight write. Expired claims and the `Resume` lane do not block.
6. Otherwise the target goes live.

#### A resuming recipient

The parent-only resume doorway names one ended child explicitly. Dispatch keeps that row for reply preparation, uses the rollup with context, and parks without resolving a live pane. The queue commit then decides join-or-start under the workspace lock, after all dispatch preflights:

1. If the card became live, it writes nothing, and the doorway retries ordinary dispatch.
2. Otherwise it reads the clock and the card's queued registration waits. A wait counts only while the child is absent or ended no later than its enqueue, and while its `register_by` is strictly later than now.
3. A follow-up that joins such a wait copies the earliest deadline and opens no pane.
4. With no such wait, it starts a resume with `register_by = enqueued_at + 2m` (`RIMZ_MESSAGE_REGISTER_TIMEOUT_MS` overrides the limit), and the doorway opens a pane under the zone lock.

A later end, the cancellation of every waiting follow-up, or the deadline itself permits a new resume, even before a sweep. The commit returns the stored record and its join-or-start outcome, so the doorway makes no separate in-flight read. A failed split errors the existing record without an unresolved-message event, and a successful opening keeps it whatever the bind check finds. The child side is [subagents.md § Follow-ups and resume](./subagents.md#follow-ups-and-resume).

#### Receipts

The receipt follows the decision:

- A live write prints `sent to @handle (msg_...)`. The record is `Sent`, and only the receiver's acknowledgment makes it `Delivered`.
- A park names its first cause, in this precedence: the scheduled opening, then the first unmet `after` condition, then the first unmet `when` condition, then the receiver's readiness, then the blocking record id or the missing pane. Status and provider-starting parks keep their steer hints.
- A native-input wait points to `rimz asks show` and `rimz answer`.
- A resume follow-up says the child is resuming, that delivery follows its registration, and that failure follows a missed registration deadline in absolute UTC. It does not depend on the three-second bind check.
- Only a live attempt that defers without a cause keeps the bare queued receipt. Interrupt keeps its delivery-deferred hint.

On the rollup-only path the receipt does not prove the pane is still live. `message show` runs the full check.

Fan-out delivers each target in turn, one message interval apart, and prefixes each delivery with the addressed handle (`@all,`) so receivers read it as a group message, even when caller exclusion leaves one peer. A member whose attempt is skipped does not stop the rest, and the summary names sent and skipped agents with their message ids.

An address that matches nothing, after the durable fallback, writes a terminal `message.errored` bounce carrying the raw address, so `message list --all` shows the failed hand-off.

## Delivery

### The ordered check

[`DeliveryCheck::verdict`](../../../crates/rimz/src/message/deliver.rs) evaluates a queued record against a fresh snapshot and reports the first blocker. The order is the contract: `rimz message show` renders it, the sweep backs off on it, and the delivery helper re-runs it before claiming.

| # | Check | Verdict when it fails | What releases it |
| --- | --- | --- | --- |
| 1 | A queued automatic command is still within its validity window | `Expired` | Nothing. The next reconcile records the terminal outcome |
| 2 | A queued follow-up with `register_by` has a live receiver row or one ended after enqueue | `WaitingForRegistration` | Registration, a later end (check 8), or registration expiry |
| 3 | `not_before` has passed | `Scheduled` | The clock, through the host sweep |
| 4 | Every `after` condition is met | `WaitingOnAfter` | The referenced agent reaching its gate with no deliverable queued work |
| 5 | Every `when` condition is met | `WaitingOnWhen` | The watched agent completing its dwell in the raw status |
| 6 | The record is the oldest holder of its card's lane | `BehindFifo` | The blocking record settling, or a blocking `Sent` prompt's acknowledgement or hold deadline |
| 7 | The receiver card exists in the snapshot | `ReceiverGone` | The agent reappearing, or GC archiving the record |
| 8 | The receiver has not ended | `ReceiverEnded` | The sweep archives open records for the card and unmet `when` conditions watching it |
| 9 | Not inside the compaction window | `Compacting` | `CompactionEnded`, or the 90 s window expiring |
| 10 | The gate is open for the effective status | `GateClosed` | The agent reaching `Idle`, `Success`, or `Sleeping`, plus `Failed` under `--on any` |
| 11 | The resumed provider has started | `ProviderStarting` | Its next lifecycle observation (normally `Registered`), or one delivery window. Lazy-registering providers skip this check |
| 12 | A `Resume` gate's park is recoverable | `ResumeUnrecovered` | The budget window resetting, or the overload marker clearing |
| 13 | No open blocking prompt reserves the input | `AskWaiting` | Answering the ask, or `--force` |
| 14 | A live pane can receive a paste | `NoPane` | A bindable pane appearing. A pinned record waits for its pinned pane |

All fourteen pass and the verdict is `Ready`.

Checks 4 and 5 pass for a condition satisfied in the snapshot even before its stamp is written, so `message show` can read a record as past its conditions while it still waits. FIFO selection and the claim use the durable stamps (`MessageRecord::is_deliverable`), which the sweep writes.

Check 14 honours the pin. A record pinned to a pane binds only to that pane (`address::pane_binding`), and a `NoPane` deferral keeps the pin (`Store::defer_message_wake`). Only a requeue after a failed attempt clears it (`MessageRecord::requeue`). A pane is bindable when it reaches `agent_panes`, which the snapshot builds from the panes card admission keeps ([sidebar.md § Presence model](../sidebar/sidebar.md#presence-model)). A pane the fold drops is a receiver no message can reach, however healthy the agent's record looks. Without an end stamp, `NoPane` can outlive every other gate. A durable end lets the sweep archive the records.

#### The FIFO hold

Check 6 counts three kinds of holder in the same card and lane: a deliverable `Queued` record, an unexpired `Claimed` record, and a `Sent` prompt still inside its reconcile deadline. The selector and the explain scan share this rule (`crates/rimz/src/store/message.rs::delivery_batch_indices`, `older_ready_blocker`).

A `Sent` prompt holds the slot while its turn-start acknowledgement is pending, even if the card still reads `Idle`: the store already knows a prompt was submitted before the hook commits. Readers release an overdue hold without a sweep or a record transition, through `MessageRecord::sent_hold_expired`. A compaction, ingress, or provider-start deferral extends that deadline ([Reconciliation](#reconciliation)). Late acknowledgements stay valid while the record is `Sent`, or `Queued` after a requeue.

A `Sent` command does not hold the card. The composer queues a paste made during compaction, and holding for the command's 180 s acknowledgement window would freeze prompts whenever its hook never arrives.

Scheduled, condition-blocked, and `Resume`-gated records are left out of the FIFO scan, so they never block a later record that could deliver now. Resume nudges have their own lane, so a wakeup never queues behind user text that cannot deliver until after it. Steer and interrupt bypass FIFO, the prompt hold included.

#### Ended receivers

An `Ended` receiver refuses delivery until a later lifecycle fold revives its card. Pane attach does not revive it. The exec wrapper's `rimz.agent-resumed` stamp, appended on every resume, does. A record queued to an ended card before its resume, such as a cohort resume's positional prompt, needs that stamp. The append does not run the hook reactor, so the wrapper triggers delivery itself ([What triggers a delivery](#what-triggers-a-delivery)).

A queued follow-up with `register_by` waits for registration while its card is absent or every matching row has `ended_at <= enqueued_at`. A tie keeps waiting, so the record is not archived against the child's previous end. A child that registers and then ends after enqueue reaches `ReceiverEnded` instead, and the sweep archives the record with the ordinary receiver-ended reason. While registration is awaited, the sweep wake is deferred by at most one delivery window, capped at the deadline. `Store::reconcile_stale_messages` alone writes `Expired` at or after the deadline, under the same card-aware predicate, with a reason naming the child and the deadline. A live row wins even past the deadline if reconciliation has not yet expired the record, and a registered child waiting on its own prompt does not expire.

Ended launched children stay in runtime snapshots while their parent is visible and has no parent of its own (`crates/rimz/src/store/runtime.rs::RuntimeProjection::runtime_from_refs`), so an ended grandchild drops out under a visible parent. For an ended receiver that projection omits, the sweep and `message show` consult the audit row instead of reading its absence as an unknown receiver. For `ReceiverGone`, `message show` consults the audit rollup too. When the durable card survives but runtime projection expelled it, the verdict names the card's last-seen time, says no live process claims it, and points at `rimz agents resume`. The verdict and its JSON name stay the same.

#### Compaction and gates

The compaction window at check 9 closes every gate, `Resume` included. A receiver whose `compacting_since` marker is less than 90 seconds old (`COMPACTING_WINDOW_SECS`) takes nothing. The window expires so that a lost compaction-end signal costs a delay instead of a wedged queue. Stale-`Sent` reconciliation uses the full compaction bracket instead ([model.md § The compaction bracket](../agents/model.md#the-compaction-bracket)).

`DeliveryGate::Resume` has no flag. Auto-continue stamps it on its own nudge, and check 12 re-verifies at delivery time that the park is still resumable. `Done` and `Any` records stay parked while an agent is paused, so a rate-limited agent does not receive a pile of user text the moment it waits. Both open for `Sleeping`, so a resting agent with an armed one-shot wait can receive a message without consuming that wait. `Resume` does not open for `Sleeping`. Hibernation does not change the gate's status input, and delivery thaws the provider before writing to its pane.

### What triggers a delivery

Four paths converge on the same delivery helper.

**Lifecycle hooks.** The lifecycle reactor declares [`DELIVERY_CHECKPOINT`](../../../crates/rimz/src/agents/lifecycle/event.rs) as `Registered`, `TurnEnded`, `TurnInterrupted`, and `CompactionEnded`. On one of those it finds the FIFO head for the event's card and spawns a detached `rimz message deliver --message-id <id>`. Registration releases messages parked while a resumed provider starts. Subagent stops and compaction starts do not check the queue.

The same reactor nudges the sweep when the event's agent is referenced by an unmet condition: `after` conditions on `DELIVERY_CHECKPOINT`, and `when` conditions on the wider [`CONDITION_CHECKPOINT`](../../../crates/rimz/src/agents/lifecycle/event.rs), which adds `TurnStarted`, `AwaitingInput`, and the subagent edges because a dwell can start or break on any of them. Both actions run after the `LifecycleEvent` commits, and the helper re-checks durable state before claiming.

**Resumed wrappers.** After the provider spawn, `record_own_resume_pane`, and the stage re-wake, every exec wrapper with an attach target selects that card's FIFO head through `message::spawn_queued_head`, the selector the lifecycle reactor shares, and spawns the detached deliverer. A Stage notice the re-wake parked behind the head rides the same batch, with the resume prompt first. A host sweep between the resume stamp and the re-wake can still claim the prompt alone; the notice then follows at that turn's end. This covers parent follow-ups and ordinary resume prompts alike, and lazy providers with no registration hook. A re-wake error, failed queue read, or helper spawn failure logs and does not fail the resume; the trigger still runs after a re-wake error. The host sweep remains the backstop.

**The host sweep.** The room's sidebar host spawns `rimz --mux <mux> message sweep --workspace-id <id>` when the wake stamp comes due ([Scheduling and wakeups](#scheduling-and-wakeups)). Its argv names the room and mux because the host inherits no pane environment.

**Auto-continue.** When a persisted park reaches its reset or backoff condition, the producer spawns `rimz agents auto-continue`, which queues a `Resume` message (or reuses the existing one) and calls the same helper ([providers.md § Auto-continue](../agents/providers.md#auto-continue)).

### The delivery helper

`rimz message deliver --message-id <id>` is a hidden helper, never run by hand.

1. **Settle.** Sleep 400 ms (`RIMZ_MESSAGE_SETTLE_MS`) so the agent's state stabilizes after the hook.
2. **Re-check.** Run the ordered check against a fresh snapshot. The hook's choice is a hint, and this check decides.
3. **Claim.** Under the workspace lock, refuse a head behind an older holder in its card's lane ([the FIFO hold](#the-fifo-hold)). Otherwise move the compatible FIFO [batch](#batching) from `Queued` to `Claimed` and bump each `attempts`, in one transaction.
4. **Write.** Send through the [pane write](#writing-to-the-pane). If [smart compaction](#smart-compaction) fires, type the compact command alone and release the prompt batch back to `Queued` without an attempt penalty.
5. **Record.** A successful write moves the batch to `Sent`, live until confirmed.

A failure before any byte is written returns the record to `Queued` with `last_error` set and its pane pin cleared, so the next attempt re-resolves a pane. With `attempts` at its cap the record becomes `Abandoned` instead, and the failures with no retry path in the [terminal table](#terminal-states) become `Errored`.

`rimz message steer <id>` runs the same helper with a steer policy. It still needs the named record, a receiver card, and a live pane, but ignores `not_before`, FIFO position, the gate, and the resume-recovery check. `claim_message_for_steer` claims only the named record, keeping the claim TTL guard and skipping the FIFO compare. It is the manual escape hatch for a dependency cycle or a vanished upstream agent. A waiting ask still defers it unless the record or the command carries `--force`.

`rimz message interrupt <id>` runs `DeliveryPolicy::Interrupt { force }`, which shares steer's single-record claim, gate override, and retry handling and adds the key after the write. The record stores no delivery mode, so a parked `--interrupt` send delivers at its next checkpoint as an ordinary boundary send, without the key.

### The subagent-digest join guard

A fleet digest (`SUBAGENT_REPORT` or `AGENT_REPORT`) points a launcher at the response files of runs that have settled. A launcher that already joined those runs inline has read the results, so the digest would cost it a turn and show it nothing. Two layers cancel it.

The producer cancels first, as eager cleanup. An attended inline `rimz subagents wait` stamps `joined_at` on the answer it prints, and `rimz subagents stop` stamps every answer of the selected run. Both cancel each digest whose every listed answer is joined, and the composer rechecks after queueing ([subagents.md § Composing the digest](./subagents.md#composing-the-digest)). Any of these cancels can fail after the stamp and before the queue changes, so delivery cannot rely on them.

Delivery is the guarantee. [`attempt_delivery`](../../../crates/rimz/src/message/deliver.rs) calls `cancel_moot_message` before candidacy, and again on each claimed member before sending. For a `Harness` fleet digest, each call runs `harness::run::report::digest_fully_joined` over current and earlier answers whose `report_message_id` is this digest. Reopening a child keeps that link on its unjoined earlier answer.

> **A digest with at least one linked answer, every listed answer joined, never reaches the pane.**

When either scan finds that, the guard cancels the digest with reason `joined before delivery` and skips the write. It skips the write even when producer cleanup already removed the record and the cancel finds nothing to cancel. A digest with some rows unjoined, or with no linked rows, delivers its full text, so a genuinely unread result still reaches the parent. Other harness notices take ordinary delivery, subject to the [run-link guard](#the-run-linked-notice-guard).

A guard that cannot answer never sends. When the run scan or the cancel fails, the helper warns with the message id. Before the claim it stops. After the claim it calls `release_message_claims` to return the digest to `Queued` without an attempt penalty, and stops. That queue commit refreshes the wake stamp. An error from the release propagates, but an unreadable run file does not abort the sweep. The queued digest keeps its FIFO head position, like any head that cannot deliver.

One race remains. A join after the post-claim scan can still race the send, because the store's workspace lock does not span pane I/O (the per-pane write lock does, and it serializes writers, not store transitions). The join-side cancel accepts a `Queued` or `Claimed` record and leaves a `Sent` one for confirmation, since a cancel cannot retract a paste. A cancel that lands while the paste is being written stands: the sender's `Sent` mark finds no live record, warns with the message id, and the record stays `Canceled`. When the guard cancels a record named by `message steer`, the command exits 1 with `message <id> is no longer queued`.

### The run-linked notice guard

The pause and stall helpers set `MessageRecord.about_run` to the child run they claimed before queueing their notices. `cancel_moot_message` checks any record carrying that link, independently of its sender or notice kind. The run-link check adds no run read for an unlinked record; digest scans remain unchanged.

Before candidacy, the head's linked run is read even while the parent's turn-boundary gate is closed. After the claim, every claimed member passes the same check. A terminal run cancels its record through `Store::cancel_message` with reason `child run <status> before delivery`, using `RunStatus::as_str`; a missing run cancels it with `child run gone before delivery`. No pane write follows, even if the cancel finds the record already closed. `rimz message show` keeps the reason in its timeline. When the guard cancels a record named by `message steer`, the command exits 1 with `message <id> is no longer queued`.

A nonterminal run leaves the record deliverable. Any run-read error other than `RunStoreErr::NotFound` warns with the message and run ids and also leaves it deliverable, without releasing its claim. Unlike a digest, an advisory notice must not hold the parent's FIFO lane on an unreadable run file. A failed cancellation still defers the attempt.

Linked notices remain batchable. Moot members are canceled, and the survivors form the paste: two or more share a `batch_id` naming the first survivor, while a lone survivor has none. If none survive, delivery stops without a pane write.

The link names a run, not a park episode. A child that resumes while its run stays open can still produce a notice, and an ended run reopened by a follow-up before the check reads open again. Neither case withdraws the old notice. A transition after the final check can still race external pane I/O, as with the digest guard above; a `Sent` notice is never retracted. An older binary can also drop the link on a queue rewrite ([mixed binaries](#mixed-binaries)).

### Batching

When a queued head delivers, the helper extends the claim through the contiguous ready prefix of the head's lane, so messages that piled up during one turn arrive as one interaction. A record joins the batch only if it:

- is a `Prompt` that submits with Enter and does not start with `/`
- has its own gate open
- matches the head's `force` flag
- shares the head's batch key: the sender's channel for an `Agent`, the receiver's channel for a `Human`, `Subagent`, or `System` sender, and no key for a `Harness` sender

The first record that fails a rule ends the batch. `Resume` control messages never batch.

A fleet digest is always claimed alone, as its own head. Nothing else would stop it: a digest is an Enter-submitted `Prompt` with no batch key, so it could ride behind a channel-less human message. Other harness notices batch normally, `TEAM_REPORT` and run-linked pause/stall notices included. Every claimed member passes the [retirement guards](#the-run-linked-notice-guard) before the paste.

The batch lands as one paste and one submit. Attributed members keep their own [header](#the-message-header), system members stay verbatim, and a blank line separates sections. Claim, `Sent`, release, and pre-send failure each change the whole batch in one queue transaction.

### Confirmation and retry

A `TurnStarted` hook aligns the submitted prompt against the candidate `Prompt` batches for its card through [`align_submitted_prompt`](../../../crates/rimz/src/store/message.rs). Record text supplies the boundaries between adjacent messages. Attributed and harness records match through their structured headers, and system records match verbatim. Inside a batch the text and the blank-line joins must match exactly. Only the outer whitespace follows the hook payload's normalization.

| Record state and event | Result |
| --- | --- |
| `Sent` + submission containing the batch exactly | `Delivered` |
| `Sent` headered batch + submission with surrounding text | `Delivered` with a mixed-submit reason and the stray byte counts. The surrounding text becomes direct-input transcript entries |
| `Sent` system batch + submission with surrounding text | No transition. Headerless text needs an exact whole-prompt match |
| `Sent` + submission without the batch | No transition. The submission is direct pane input |
| `Queued` after an unconfirmed write + correlated submission | `Delivered`, with no time limit |
| `Claimed` + any submission | Ignored. The in-flight write owns the pane |

Stray composer text around a headered batch is never a reason to write the pane again. A headerless system batch needs an exact match because its text alone cannot tell a delivery from the user's own words.

When a turn-start adapter reports no usable prompt text, confirmation selects the oldest `Sent` prompt batch with at least one record still inside its hold deadline. If every candidate batch has expired, it falls back to the oldest, which preserves late acknowledgements. Selection confirms the whole batch, expired members included. A `Compacting` hook selects the oldest `Sent` batch of `Command` records regardless of age.

Correlation never selects a `Claimed` record. That leaves one duplicate window between the claim and the write's acknowledgement: settling inside it could not retract the in-flight paste, so the active deliverer keeps ownership.

#### What reads the confirmation

Origin classification is a separate, read-only decision in `store::message::classify_submitted_prompt`. Before confirmation removes records from the live queue, the hook reads `Store::list_messages` for this card's `Sent` or `awaiting_late_ack()` records, of either body. After confirmation, the classifier combines that view with the confirmed records. An unmatched headerless section can recover its sender and causal ids from a headerless in-flight record with the same trimmed text. That does not acknowledge the record: a system command stays `Sent` at turn start and waits for `Compacting`. The [transcript writer and spend consumer](./transcript.md#writing-entries) share the classified sections, and neither widens the acknowledgement contract.

`Store::confirm_delivered_for_card_with` hands the exact selected records to a callback inside the workspace lock, before the queue write. `DeliveryAckMatch` tells `PromptCorrelated` from `OldestSentBatch`, which a blank or absent prompt and compaction use. `confirm_delivered_for_card` passes a no-op callback. A callback must not reacquire the workspace lock or let an enrichment failure fail the acknowledgment. The one shipped callback is `harness::run::record_run_delivery`, which enrolls a launcher's message into a peer's open turn or [stamps a subagent's follow-up](./subagents.md#a-follow-up-reopens-the-run). Its rules are in [subagents.md](./subagents.md).

#### Reconciliation

Reconciliation (`Store::reconcile_stale_messages`) runs in `message sweep` and `gc`, and handles expired claims alongside stale writes.

A `Sent` record has one deadline for reconciliation, wakeups, and readers: `retry_after` when set, otherwise its last write (`last_sent_at`, falling back to `updated_at`) plus its body's delivery window. A pass before that deadline leaves the record untouched, even if the condition that armed a hold has already cleared. Past the deadline, reconciliation tries each rule below in order and applies the first that matches.

| # | Case | Effect |
| --- | --- | --- |
| 1 | Expired claim: `Claimed` with no completed write within `CLAIM_TTL` | Clear the claim stamp, `pane_id`, and `batch_id`, keep `attempts`, and requeue with `message.queued` and `claim expired; sender gone`. With `attempts` at `MAX_DELIVERY_ATTEMPTS`, `Abandoned` instead, which ends a crash loop |
| 2 | Compaction bracket open on the receiver | Hold the `Sent` prompt or command and push `retry_after` one body window ahead |
| 3 | Pending ingress: an unapplied hook frame of the receiver's provider kind, captured in `[last write, last write + body window)` | Hold, push `retry_after` one body window ahead, and set `last_error` to say hook drain lags the acknowledgement window |
| 4 | Provider still starting: a prompt to a card with `resumed_at`, written at or after that stamp, within ten prompt windows of the resume | Hold and set `retry_after = now + window`, with no change to `last_error` |
| 5 | Unconfirmed prompt below the requeue cap | Clear `pane_id` and `batch_id`, keep `last_sent_at`, increment `unconfirmed_sends`, record `delivery unconfirmed; re-queued`, and requeue through the normal FIFO path. A requeued batch member forms a new batch next time |
| 6 | Unconfirmed prompt at the cap | `TimedOut` |
| 7 | Unconfirmed command | `TimedOut` with `delivery unconfirmed; command not resent` |

The holds (rules 2 to 4) move no counter and write no audit event, and readers honour the deferred deadline after the condition clears. Each exists because a resend there would do harm:

- **Compaction.** A composer queues a paste made during compaction and submits it when compaction ends, so a resend would become a second turn.
- **Pending ingress.** The acknowledgement may already be captured and waiting in the hook ingress. Claimed but unapplied frames count. The read runs once per reconcile that has a stale `Sent` record, starting at the applied cursor, and a read failure warns and falls through to rules 5 to 7. A CLI sweep or gc that holds a record on ingress starts or nudges the hook drainer, and a failed wake warns without failing reconciliation.
- **Provider start.** A resumed provider can take longer than one window to submit. The hold covers every provider kind and steer and interrupt writes too, but never commands or writes sent to the process before the resume. The bound is five minutes by default and scales with `RIMZ_MESSAGE_DELIVERY_WINDOW_MS`, and window zero holds nothing. The next lifecycle observation clears the resume stamp. The hold runs before the requeue cap, so a prompt at the cap still waits. A correlated acknowledgement during the hold confirms `Delivered` with no extra unconfirmed send, and an ordinary startup hold adds no doctor warning.

The pending-ingress hold has no time limit, but its frame range is fixed. It runs from the last write, inclusive, to last write plus body window, exclusive, and retry hints never move that bound. The hold therefore ends once the backlog captured inside the window is applied, however much same-kind activity follows. Matching by provider kind instead of card avoids duplicating adapter identity resolution in the store, at the cost of delaying an unrelated same-kind resend by that drain lag.

An applied hook event's `timestamp` is its frame's ingress capture time, not its apply time, and the ingress match compares capture time. To read a frame's drain lag from the events log, subtract the ingress id's milliseconds from the event id's.

A command reaches the pane at most once, because a duplicate `/compact` can discard context and a missing acknowledgement does not prove the first submit failed. Confirmation clears `last_error`, so a held or requeued record that is later confirmed reaches history as `Delivered` with no failure sentence.

A record whose agent has simply not reached a qualifying boundary is not failing. It stays `Queued` and no counter moves.

## Writing to the pane

`mux::PaneWriter` holds one per-pane advisory lock, at `RuntimePaths::pane_write_lock`, across a whole write batch: any compact-first command, every typed segment and pacing delay, the prompt paste, the `Sent` transition, the final Enter, and for interrupt the delay and the key. `rimz answer` and `rimz pane send` take the same lock, from any workspace. Different pane ids are independent, and identical Zellij pane ids in separate sessions share a lock, conservatively. Steer waits for the lock instead of preempting another writer. Acquisition times out after the 30-second workspace lock timeout as a mux error and follows the ordinary send-error recovery.

The pane lock and the [FIFO hold](#the-fifo-hold) protect different things. The lock keeps two writers' bytes apart. The hold keeps a fresh boundary send from submitting while an earlier prompt's acknowledgement is pending.

`PaneWriter::open` requires the session its writes address, and every writer passes the resolved room's session, so a write reaches that room's Zellij session from outside any pane. `rimz pane send` first checks that the room holds the pane, because a raw pane id reaches it with no lookup. `rimz message`, `rimz answer`, and the budget parker take their target from a fresh listing of that session. tmux ignores the session, because its pane ids are server-global.

### Paste, then submit

A `Prompt` is wrapped in bracketed-paste markers (`ESC[200~` to `ESC[201~`) through `MuxBackend::paste_text`, then Enter is pressed as a separate `send_key`. The close marker is what separates text from submit. A composer that receives text and a trailing `\r` in one PTY read treats both as pasted content, with the `\r` a literal newline. The composer leaves paste mode on `ESC[201~`, so the following Enter is a keystroke even when every byte arrives in one read. Adding a delay does not fix a submit problem on this path. Explicit markers work even when the application has not enabled bracketed-paste mode.

Inside the paste, LF and CRLF line endings travel as CR. Composers normalize CR to a newline but drop a bare LF, and terminal emulators and tmux's `paste-buffer` send CR for pasted line endings too, so multi-line prompts land multi-line.

The backends carry a large paste differently:

- **tmux.** A tmux command frame cannot carry a full message body as an argument. RimZ loads a uniquely named buffer holding the opening marker, the CR-normalized body, and the closing marker through stdin, then pastes it with `paste-buffer -d -r` in the same invocation, so markers and body take the same PTY route even in copy mode. On tmux 3.7 and newer, `-S` turns off control-byte sanitization ([tmux-reference.md § Pane I/O](../../externals/mux-adapter/tmux-reference.md#pane-io)). `-r` keeps the byte stream raw and `-d` deletes the buffer. A failed batch deletes it best-effort before returning the original error.
- **Zellij.** RimZ writes the same bracketed byte stream in ordered chunks of at most 8 KiB (`ZELLIJ_WRITE_CHUNK`). On the first failed body chunk it stops, writes the closing marker best-effort, and returns the original error. The per-pane lock spans every chunk and the cleanup, but cleanup cannot retract delivered fragments or guarantee closure while the backend is down.

### The interrupt key

Interrupt is paste first, key second, inside `send::send_batch_to_live_pane` under one `PaneWriter`.

1. With the lock held, it rereads `store.snapshot_cached()`. Only a live `Running` or `Waiting` row gets the key, because on a resting row the paste itself starts the turn the key would cancel. The awaiting-input refusal without `--force` comes from dispatch's preflight and from `write_batch`'s check against the earlier bound row.
2. It writes exactly as steer does: any smart-compaction command, the paste, the `Sent` record, Enter.
3. After `RIMZ_MESSAGE_INTERRUPT_DELAY_MS` (default 3000) it presses the adapter's key once, still under the lock.

A clean turn parked on background work already projects to `Success` in `rested_status()`, so it skips the key as a resting turn.

The key does not guarantee a fresh turn. Codex's expanded question editor, for one, takes pasted text as an answer ([codex-reference.md § Async questions](../../externals/agent-adapter/codex-reference.md#async-questions-observed-on-01580)).

Only Codex declares `confirm_interrupt`. A durable event follower starts before the key. After the pane lock is released, delivery waits up to `RIMZ_MESSAGE_INTERRUPT_CONFIRM_MS` (default 5000) for a `TurnInterrupted` matching the kind and the live card's session id (queued records can keep a provisional launch id). A later `TurnStarted` cannot erase that evidence. Both CLI paths report an unconfirmed stop on stderr and leave the stdout receipt and exit 0 unchanged, because the text was already sent. Reply waits keep their own receipt stream and exit status. An unconfirmed stop can mean the turn is still running or that the optional Interrupt hook is untrusted. Confirmation is transient, never stored, and proves no restart. Claude and resting agents do not wait.

Two hazards are accepted. The budget parker may press Escape at the same instant, which produces a double Escape. A stopped Codex turn does not mean every descendant process exited.

### Commands are typed

A `Command` body takes the raw type path, because a composer treats pasted text as literal content and a pasted `/compact` would land as a prompt. The raw path is byte-faithful with no close marker, so timing does the separating work that the marker does for prompts. A composer can reassemble fast typed characters into a synthetic paste and suppress an Enter that follows the burst ([Codex's paste burst](../../externals/agent-adapter/codex-reference.md#paste-burst)). RimZ therefore waits the command-submit delay (1 s, `RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS`) between typed segments and before Enter.

A command with arguments is typed in two segments. When the adapter appends an instruction to its compact command, RimZ types the declared command and its trailing space first, then the arguments one submit delay later. A composer can classify one long typed chunk as pasted text, and typing the command separately keeps the slash literal so it dispatches, with the paste-classified remainder as its trailing text. A bare command, multi-word plugin commands included, is one segment.

`rimz pane send` also uses the raw type path, since a bare shell would print the paste markers literally.

The send path spaces messages, never a paste and its submit. It sleeps one message interval (1 s, `RIMZ_MESSAGE_INTERVAL_MS`) before each message after the first, so fan-out members and a compact-then-prompt pair reach the composer as separate events.

### The Sent-before-submit barrier

[`write_batch`](../../../crates/rimz/src/message/send.rs) records the batch as `Sent` after all its text lands and before it presses Enter. A submitted message is therefore always preceded by its durable record and audit event. The reverse order would let an agent start a turn RimZ has no record of.

A crash between text and submit leaves a `Sent` record whose text sits unsubmitted in the composer, which reconciliation handles. A record that a cancel, clear, or archive finalized during the write is not recreated as `Sent`. The write logs a warning naming the message ids and still presses Enter, since the paste cannot be retracted and text left in the composer would glue onto the next write.

### The message header

Every attributed prompt carries a header before the record text:

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
| `SUBAGENT_PAUSED` | A launched child parked on a provider limit, queued for its parent once per park and withdrawn if its run ends before delivery ([backstops](./subagents.md#backstops)) | `@rimz` |
| `SUBAGENT_STALLED` | A launched child silent mid-turn or not yet started past the same stall window, queued for its parent once per run and withdrawn if its run ends before delivery ([backstops](./subagents.md#backstops)) | `@rimz` |
| `MESSAGE_UNDELIVERED` | An agent-sent message ended without delivery, sent once to its live sender | `@rimz` |
| `MESSAGE_QUEUED` | An agent-sent message stays queued past its receiver-class delay, sent once to its live sender | `@rimz` |
| `DEADLINE` | A deadline-ladder rung steered to the supervised child | `@rimz` |
| `WAIT` | A timer, command, watch, or clock wait delivery | `@rimz` |
| `CACHE_KEEPALIVE` | A neutral prompt-cache ping ([policy](#prompt-cache-keepalive)) | `@rimz` |
| `SIGNAL` | Every delivery fired by a `Trigger::Signal` row | `@rimz` |
| `STAGE` | A prose-only stage-open delivery from a flip or a resume or registration re-wake | `@rimz` |
| The notice name, upper-cased | A harness notice this binary does not know (`HarnessNotice::Other`) | `@rimz` |

Commands carry no header, whatever their sender (`MessageRecord::ships_headerless`), and neither do system records or `--no-from` sends. An unknown harness notice keeps its string and takes ordinary harness delivery ([store.md § What is in it](../store.md#what-is-in-it) explains why the string survives).

A handle gains `#channel` when the delivery crosses lanes. The recipient's lane comes from its registered channel, its live pane channel, or the addressed channel, so a just-launched teammate in the same lane does not gain a spurious suffix before pane capture lands.

`address::agent_sender_handle` uses `agents::petname::sender_handle`: role, else explicit or pet name, else kind. Unlike the [listing handle](./fleet.md#petnames-and-the-canonical-handle), it never depends on a profile's or kind's current uniqueness. A matching live peer supplies identity and channel, and otherwise the stored sender fields do. A bare-resumed launched agent still carries a minted name recovered through ancestry. The kind fallback is for senders with no role or name, such as a hook-registered agent or an unknown parent.

The receiver's turn-start hook parses the header into transcript entries ([transcript.md § Writing entries](./transcript.md#writing-entries)), for the types `store::message::classify_header_line` recognizes. That list leaves out `SUBAGENT_PAUSED`, `SUBAGENT_STALLED`, `MESSAGE_UNDELIVERED`, `MESSAGE_QUEUED`, and unknown notices. A correlated record of one of those types still recovers its origin through `classify_submitted_prompt`, but its entry keeps the header lines in its text. `store::message::parse_sender_handle`, shared by header parsing and prompt alignment, accepts only `From: @<handle>` or `From: @<handle> (<label>)`. The handle is non-empty and whitespace-free. The label is non-empty, whitespace-free, and free of parentheses, and nothing may follow the closing parenthesis. The label is stripped before writing `entry.from`, which leaves the bare `@handle[#channel]` reply address.

## Compaction commands

Four callers send a native compact command through a `Command` record: smart compaction ahead of a prompt, idle compaction from the sidebar producer, flip compaction at a team hand-off, and the operator's `rimz agents compact`. Routing the command through a record gives it the same claim, retry, audit, and at-most-once write as any message.

The three standalone callers (idle, flip, operator) go through `crates/rimz/src/message/compact.rs::send_compact`, which queues a pane-pinned, `Done`-gated command and attempts boundary delivery at once through `crates/rimz/src/message/synthetic.rs::attempt_now`. A miss records the refusal verdict or delivery error. A retryable post-claim stop keeps the attempt's own reason, and only a terminal stop writes the caller's fallback. The store refreshes the host's wake stamp in each queue-changing commit, so a queued command's retry does not depend on later message traffic. The sweep defers a refused ready head one delivery window; deliverable followers do not pin the stamp in the past ([Scheduling and wakeups](#scheduling-and-wakeups)).

A queued `Resume` record contributes no wake deadline: the sweep's `queue_head` excludes its control lane, so auto-continue owns its re-drive and a miss on an otherwise idle queue leaves no stamp. Claimed and sent resume records still arm recovery.

An automatic command (`body == Command && automated`) is perishable. While `Queued`, it is valid for ten minutes from `enqueued_at`. The window is derived, never stored, and `RIMZ_MESSAGE_COMMAND_VALIDITY_MS` shortens it for tests. `MessageRecord::is_deliverable` excludes an expired record at every FIFO head and blocker decision, even when no sweep runs. The next `Store::reconcile_stale_messages`, from the sweep or `rimz gc`, alone finalizes it as `Expired`, with the reason in `last_error` and `message.expired`. `message show` reports expiry before every other blocker, and steer and interrupt cannot bypass it. Prompts without a resume registration deadline, harness notices, operator commands (`automated: false`), and non-`Queued` records do not expire. A lapsed claim returns to `Queued` and ages by its original enqueue time, while a human requeue creates a new record with a fresh window. Smart compaction's released prompt can deliver on its own once its compact command expires.

`agents::compact_command` is the one composer every automatic caller uses. It picks the brief for the agent's seat: `CompactSeat::Team` when the launch-stamped `AgentState.team` is set, `Solo` otherwise. `HarnessConfig::compact_instruction` returns the configured `compact_instruction` for that seat, else RimZ's brief. `rimz agents compact` bypasses the composer only for an explicit instruction. `LaunchSpec::compact_command` renders the adapter's native command. When the adapter declares `CompactInstruction::Trailing`, it appends [`[harness] compact_instruction`](../../guide/configuration.md#smart-compaction), folded to one line because a newline would submit it, and the send path types it as the [second segment](#commands-are-typed). Other adapters receive the bare command.

### The repeat guards

Two guards stop a repeated compaction.

**The context baseline** is consulted by smart and idle compaction. `compacted_context_tokens` records the reading a compaction fired on, and every caller stamps it when the reading is known. The fold carries it into `AgentState::last_compact_command_tokens`. While a carried-forward stale gauge still equals that baseline, no further command is sent for it, and a new reading re-enables it. Without this a stuck gauge would compact on every message.

**The unprompted marker** is consulted by idle, flip, and operator compaction, through `refuse_repeat`. `AgentState::compacted_awaiting_prompt` is a durable timestamp. `AgentState::compacted_since_prompt` reads it, and `compaction_unprompted` adds an open compaction window. The fold sets the marker when:

- a compact command reaches `Sent`, even without a context reading
- a successful `CompactionEnded` reports the manual trigger (`auto: Some(false)`)
- a successful `CompactionEnded` lands at rest (status neither `Running` nor `Waiting`) with a known trigger, or closes an open compaction bracket

A close that resumes a turn leaves the marker alone, and so does a triggerless close outside a bracket, which is an echo of a compaction already folded (`store/snapshot/project.rs`). The marker survives rollup rotation, linked successor adoption, and re-registration, and clears only on `TurnStarted`. A later `Delivered` acknowledgement cannot restore a marker its prompt hook already cleared. For adapters without native turn-start hooks (plugins that declare no `turn_start`), the reducer records the marker but `compacted_since_prompt` ignores it, because pulled observations cannot durably clear it.

Delivery re-checks the marker after claiming a command and before writing its pane. When the marker is set and the agent is still compacting, the command returns to `Queued`. When the compaction already finished while the command was parked, the record settles `Errored` with the no-repeat reason.

### Smart compaction

A provider compacts on its own near its context ceiling, so a prompt sent close to it can be cut in half by a compaction that fires mid-turn. `--smart-compact` compacts first, so the prompt lands against a fresh window.

Thresholds parse as `70%` (a fraction of the window), `120000` (absolute occupied tokens), or `180k` and `1m` (suffixed counts). Dispatch fills an omitted threshold from the [`[harness] smart_compact`](../../guide/configuration.md#smart-compaction) default for every caller, scheduled loop waits included: `258k` when unset, none for `"off"`. An explicit per-send threshold overrides the setting. An unknown fill never triggers, and the text sends untouched.

A percent threshold reads the same fill gauge the sidebar card renders (`context_fill_pct`). A token threshold reads `occupied_context_tokens`, which prefers the folded statusline breakdown, then the per-call split (cache reads plus cache writes plus fresh input), then the carried `total_tokens` gauge.

The two delivery modes compact differently:

- **Boundary.** Send the compact command alone, release the claimed prompt batch to `Queued` without an attempt penalty, and let `CompactionEnded` start a fresh delivery against the new window. A parked record reads the fill at the boundary, not at enqueue.
- **Steer.** Type the compact command, then one message interval later paste the prompt. Reconciliation holds that `Sent` prompt while the compaction bracket delays its confirmation.

`write_live` queues, claims, and writes the command before `write_batch` checks the prompt batch's claims. A prompt canceled after its claim can therefore still cause its compaction.

Each fired smart compaction appends an `AutoCompact` assist record. A failed compact-first write settles the command `Errored` and fails the prompt's delivery attempt through the ordinary retry path.

### Idle compaction

Idle compaction sends a compact command to a resting seat, solo or team, with no prompt behind it. The point is to compact while the prompt cache is still warm, shortly before it expires. [`harness/idle_compact.rs`](../../../crates/rimz/src/harness/idle_compact.rs) owns the decision the producer and the helper share: `SeatPolicy::idle_compact_floor` ([`harness/cache_keepalive.rs`](../../../crates/rimz/src/harness/cache_keepalive.rs)) resolves the seat's token floor, `fire_point` turns it and the provider TTL into an `IdleWindow`, and `should_compact` checks eligibility.

The floor resolves along one path, and an explicit `off` anywhere on it ends the walk:

1. A TOML team role's `idle-compact`.
2. The first `idle-compact` up the seat's profile chain. A Markdown role's value rides its role overlay into the synthetic `<team>.<role>` profile, so `RoleBinding.idle_compact` stays `None` there.
3. `[harness] idle_compact`.

A `rimz subagents` child (`launch_depth` set) resolves no floor.

A seat is eligible when it is not a provider subagent, has a nonempty session id, and is effectively `Idle` or `Success` with no pending wait, input request, budget park, hibernation, or compaction marker (`AgentState::compacted_since_prompt`). Occupied context must reach the floor and differ from `last_compact_command_tokens`, and the adapter must expose a compact command. The board stage, PR state, and teammate activity play no part, so a team seat compacts after `Done` like any idle seat. A seat [keep-warm](#prompt-cache-keepalive) holds is skipped, on both the producer pass and the helper recheck, so the two reflexes never race on one cache window.

The marker and the resting-row close rule ([model.md](../agents/model.md)) guard RimZ's own compaction and any provider's automatic one.

#### The idle window

`fire_point` and the card clocks read `HarnessConfig::prompt_cache_lifetime` for the agent. Its newest observed cache write wins: `FiveMinutes` is 5m, `OneHour` is 1h, and `Unsplit` falls through to `configured_lifetime`. That static resolver chooses a sign-in-specific setting, then a plain provider setting, then `ContextCapability::prompt_cache_ttl(sign_in)`, then the fallback (default 1h). An explicit `off` stops the static chain but never overrides a known observation. The [configuration guide](../../guide/configuration.md#idle-compaction) owns the defaults and grammar. Load-time margin checks judge settings, defaults, and the fallback, not observed writes: an observed lifetime at or below the margin keeps its card clock but opens no compaction or ping window.

The machine-config fold stamps `AgentState.sign_in` on producer and consumer reads alike: `context.account.metered` first, then the cached account at `agent.login_key()`. True means subscription, false means API key (including per-request Bedrock/AWS billing), and absent stays unknown. Codex uses 12h for a subscription and 30m otherwise; Claude uses 1h on every sign-in. The cache half can lag one accounts probe (10m TTL; failed probes back off from 10s to 10m); a missing or unresolved login stays unknown. New context observations affect producer decisions one heavy pass later, as other context fields do. No consumer reads authentication files itself.

The `IdleWindow` opens at TTL minus `prompt_cache_margin` and closes, exclusive, at the TTL. Both timers and the helper measure from `AgentState::last_request_at()` ([activity clocks](../agents/model.md#activity-clocks)). A seat found past its cache lifetime is left alone instead of compacted cold. A missing anchor never qualifies, rows without a completed turn included.

Request anchoring leaves final generation out of the margin. `cache_timing_reaches_the_request_before_expiry` tests the consumer decision exactly at and one second before both the fire point and the TTL, assuming a 1-second producer tick, a 10-second helper spawn, and a 10-second keystroke-to-request delay. Those 21 seconds fit inside the default margin. They are runtime premises, not measured latency guarantees. A slow pass or early eviction costs a cache write, never correctness.

`refresh_heavy_lanes` loads one `SeatPolicy` per pass when any agent row has a team or a profile, and shares it with idle compaction, keep-warm, hibernation, and the pipeline lane. Otherwise it passes the empty policy, under which every seat reads the machine floor. `SeatPolicy::load`, which the helper also uses, falls back to the machine teams and profiles when the project root is absent or loading fails.

#### The producer and the helper

The host requires a bound pane and spawns a detached `rimz agents idle-compact`, which keeps store writes out of the sidebar's import graph. The request carries `agents::compact_command` for the seat, with the solo or team brief where supported.

The helper re-resolves the workspace, session, and pane, applies the shared decision to `Ctx::published_snapshot` (which folds pending waits and the activity heartbeat), and validates the command against the adapter and its configured instruction. Through `message::compact` it queues one automated `System` command with the `Done` gate, pins the pane, stamps `compacted_context_tokens` from its context reading, attempts boundary delivery, and appends an `IdleCompact` assist record. The optional `idle_after_secs` records the resolved threshold, and `rimz stats` shows it when present. A closed boundary leaves the command queued through the ordinary retry path.

Three layers make it once per idle stretch:

1. The producer's cache-class `(kind, agent_id)` record suppresses the same `last_request_at()` and throttles helper respawns for ten minutes. This pacing is producer-only: the helper must not veto its own spawn record.
2. The live queue refuses a second outstanding command, and `last_compact_command_tokens` suppresses the same occupied reading.
3. For adapters with native turn-start hooks, the unprompted marker stops the helper until the next turn opens, whether from a queued prompt or text typed into the pane.

Adapters without durable turn starts keep only the context-baseline guard.

### Manual compaction

`rimz agents compact @handle [INSTRUCTION]` resolves one agent with a bound pane and a native compaction command, renders the configured brief or the positional override, and calls `message::compact`. An explicit instruction is refused when the adapter does not accept trailing text, and without one those adapters get the bare command. The verb refuses adapters without native turn-start hooks (plugins that declare no `turn_start`), since the strict no-repeat guard cannot work for them.

`message::compact::refuse_repeat` refuses when any of three states holds: `is_compacting(now)`, a non-terminal `Command` record in the card's live queue, or `compaction_unprompted`. `send_compact` then queues the command and attempts delivery. A closed gate records a retry, so a lifecycle checkpoint or the host sweep delivers it later. When occupied context is known it also stamps `compacted_context_tokens`. The record carries the human or calling-agent sender with `automated: false`, so it leaves a durable message and audit event but no assist record, and it never expires.

## Prompt-cache keepalive

The keepalive pings a resting agent shortly before its provider's prompt cache expires, so that the next real turn reads a warm cache. [`harness/cache_keepalive.rs`](../../../crates/rimz/src/harness/cache_keepalive.rs) runs beside [idle compaction](#idle-compaction) on the heavy refresh lane, and `harness.cache_keepalive = false` turns it off.

An agent qualifies for a ping (`should_keepalive`) when all of these hold:

1. It is effectively `Sleeping` (a pending wait, timer waits included), or keep-warm [holds](#the-keep-warm-hold) it.
2. It passes `pingable`: not a provider subagent, a nonempty session id, not compacting, not compacted since its last prompt (`AgentState::compacted_since_prompt`), not budget-parked, not hibernated, and not awaiting input.
3. `HarnessConfig::ping_lifetime` admits its resolved lifetime: above `harness.prompt_cache_margin` (default one minute) and at least `harness.keep_warm_min_ttl` (default 15m, `off` removes the floor). The same floor applies to sleeping and keep-warm pings. `now - last_request_at` must be inside `[TTL - margin, TTL)`.
4. The run of pings has not reached `harness.cache_keepalive_max` ([the cap](#the-cap)).
5. The producer finds a live pane for it.

No team or context-size gate applies. A window missed entirely is skipped for good: the `< TTL` bound means no late ping fires, and pings for that sleep end until the agent's next turn moves the anchor.

### The keep-warm hold

Keep-warm extends the same reflex to an agent with nothing pending, for a configured horizon after its turn ends. `SeatPolicy` resolves the horizon from the trusted effective definitions:

- A team seat resolves `max` and `while` per field through `Team::keep_warm`, role over team, where `off` cancels.
- A solo seat takes its profile's `max`. A TOML profile with no `keep-warm` of its own takes the nearest one up its `agent` base chain, and an explicit `off` stops the walk. Markdown definitions arrive with inheritance resolved.
- A `rimz subagents` child (`launch_depth` set) never resolves a horizon.

The horizon is admitted only when `ping_lifetime` admits the agent's lifetime, under the same margin and floor as sleeping pings. `SeatPolicy::horizon` evaluates a resolved `while` with the shared `WhenExpr` evaluator against the seat's `lane_path`. Only `team.stage` is admitted, at load in `prepare_team`. A false known reading returns no horizon, and an unknown reading passes. A while-only hold takes `harness.cache_keepalive_max` as its horizon, and has none when that cap is off.

The horizon is anchored at `AgentState::turn_ended_at`, which a ping never moves. A ping-only turn is not agent activity ([model.md § Edges](../agents/model.md#edges)): its open and close leave status, phase, activity, turn stamps, turn ids, team signals, and the supervised run as they were, and stamp only `pinged_at`. The lifecycle hook skips its activity heartbeat, active time, and run settle on those edges too.

`cache_keepalive::holds` is the one predicate that says whether keep-warm holds an agent. It returns the horizon when every condition holds:

1. `now < turn_ended_at + horizon`.
2. The cache is still warm, `now < last_request_at + TTL`, so a missed window is dropped and never pinged cold.
3. No `--when-idle` stop is pending.
4. `harness.cache_keepalive` is on.
5. The TTL is still admitted under the current `keep_warm_min_ttl`.
6. The effective status is `Idle`, `Success`, or `Sleeping`.
7. The agent rests in the durable lifecycle (`AgentState::rests_in_lifecycle`: raw `idle` or `success`, or a clean end parked on background work). A `running` row that only a provider settle marker rests is not held, because the event fold cannot see the sidecar and would fold its ping as work.
8. It passes `pingable`.
9. The cap still permits another ping.

A hold is therefore a horizon the pings can still maintain, never a merely configured one. Idle compaction asks the same predicate and skips a held agent. An agent that is not held, whatever horizon it declares, compacts on its normal window.

The producer publishes the horizons its TTL and `while` gates admit to `lanes/keep-warm.json` each heavy pass ([state.md](../sidebar/state.md)). Enrichment applies `holds` to each published horizon on every fold and stamps `AgentCard.cache`, so a renderer learns the hold without resolving definitions, and a status or harness setting change ends it without waiting for the heavy lane. A board flip reaches the card on the next heavy pass, the same pass that decides pings.

### The ping

The producer takes the runtime lock `cache-keepalive.lock`, and records the request anchor in a per-agent runtime file before it spawns the hidden `agents cache-keepalive` helper. The sidebar and one-shot refresh paths therefore share one claim per session and anchor, even when they race. A failed spawn consumes that claim.

The helper rechecks the anchor, eligibility, horizon, and pane on `Ctx::published_snapshot`, pending waits and the activity heartbeat included. It sends a `CACHE_KEEPALIVE` notice that begins `Cache keepalive, no action needed. Waiting on:`, with one facts-only line per wait and no instructions. A keep-warm ping with no waits is the bare `Cache keepalive, no action needed.` The wait lines draw on the pending waits enrichment builds, `Subagent` and `Team` waits included ([model.md](../agents/model.md)):

| Wait | Line carries |
| --- | --- |
| Subagent | Elapsed age, last activity, and any deadline, or the terminal status with `reporting` |
| Team | Elapsed age and board stage |
| Catalog wait | Its trigger or command, and elapsed age when known |

Durations are floored to whole seconds, minutes, hours, or days. Delivery uses the `Done` gate, and a miss is finalized as `Errored`, never left for a later boundary. Every attempted delivery appends a `CacheKeepalive` assist, failures included, with `horizon_secs` when keep-warm was holding. A ping turn advances the anchor, which allows another ping one window later.

A ping turn in which the agent does work folds into its parked supervised run as a wake turn would: the first tool hook clears the park and seals its prior report into the answer ([parked runs](./scripting.md#parked-runs)). A ping with no work leaves the run parked and its answer untouched.

### The cap

`harness.cache_keepalive_max` (default 6h, `off` for none) bounds any unbroken run of pings, Sleeping or keep-warm alike, and a horizon never outlives it.

Its clock is `AgentState::keepalive_since`. The fold stamps it at a ping-only turn's open, from the prior `keepalive_since`, else the prior `last_request_at`: the agent's last request before the run of pings. A ping to a row parked on background work counts too, since its open is the same inert edge. Any other turn start clears it, as does any context reset. That covers a human prompt, an agent message, a `STAGE` or `WAIT` wake, a batch that mixes a keepalive with anything else, and a turn that resumes a parked row. The run's last ping is `pinged_at`, the one stamp a ping moves, which `last_request_at` then returns.

`run_reached_cap` says the next ping would start too late when `last_request_at - keepalive_since + (TTL - margin) >= max`. It reads stamps alone, so the producer and the helper's recheck agree. With no `keepalive_since` stamp, the first ping of a sleep is always allowed, so a `max` shorter than the TTL still yields one ping per sleep. Once it says so, `should_keepalive` refuses the ping and `holds` returns no hold, whatever the horizon. After the final ping the card shows the TTL ramp from that ping, and idle compaction treats the agent as not held.

The helper marks a ping final (`final_ping`) when `(now - since) + (TTL - margin) >= max`, with `since` falling back to `last_request_at`. It appends `Keepalive limit <max> reached: this is the last ping until your next turn.` to that ping's prompt and records `capped` on its assist. The two rules use different clocks for the last ping's start. When `now` sits just under the threshold and the ping's recorded close lands just over it, seconds of send-to-submit latency end the run without the final line.

### Best effort

The keepalive is cache retention, never durable correctness. The activity heartbeat is a latency hint. A slow refresh, no sidebar host, or early provider eviction can cost one cache write, but cannot lose a wait or its eventual wake. With an admitted lifetime and no `--check-in`, a wait on a command that never exits stays silent, and an explicit `--check-in` restores the check-in. The shared `harness.prompt_cache_margin` covers refresh, helper spawn, and submission, not an end-of-response clock. Config load refuses a margin at or above any static lifetime (fallback and every provider/sign-in combination) or `cache_keepalive_max`; that settings-and-defaults check cannot judge the agent's own later cache writes.

## Reply waits

`--wait[=DURATION]` turns a send into a synchronous scatter-gather: it stamps `reply_wait` on each record, then polls until every leg settles.

Preparation refuses what a reply wait cannot follow. Every target must be a lifecycle-bound card with installed, trusted hooks, checked once per login key. The resume doorway's explicit ended child is accepted and waits through registration and delivery, and a terminal delivery failure prints the record's reason. Broadcasts and fan-out are accepted. Create-on-miss, schedules, `--after`, `--when`, bare pane targets, and unsubmitted pastes are rejected before dispatch.

Dispatch captures one frame-aligned event-log base before enqueue and copies it into every leg, so each leg folds terminal message events from its own cursor. Each `Sent` leg anchors a skip-existing transcript cursor before dispatch, so it reads only assistant messages written after the prompt. Ordinary queued legs re-anchor when delivery starts, to exclude intervening turns. A follow-up queued to an ended, resuming child keeps its pre-dispatch anchor: there is no intervening live turn, and its reply may finish before the first delivery poll.

Each leg is a two-phase machine.

**Delivery** waits for `Delivered`, stamped by the prompt's own `TurnStarted`. A steer into a running turn opens the reply phase from `Sent + Running` instead, because the live turn emits no second `TurnStarted`: the rest of that turn is the reply. Interrupt never takes that shortcut, so `--interrupt --wait` follows only the fresh turn that `Delivered` acknowledges.

**Reply** reads status through `TurnWaitView`, which loads the loop catalog's pending waits, then the message queue, then the rollup. That is the reverse of the order a wake publishes them (message record, row consumed, turn start, delivery ack), so a wake in flight shows in at least one read. A rested agent with pending waits, or with an undelivered `WAIT` or `SIGNAL` harness message whose sent hold has not expired, reads as `Sleeping`. Legs settle through `TurnCompletion`:

| Observed | Leg |
| --- | --- |
| `Idle` or `Success` after a turn opened (`turn_started_at` set) | Completes |
| `Failed`, a delivery failure, a vanished card, or a skipped waiting input | Fails, while the other legs keep gathering |
| `Waiting`, `Paused`, or `Sleeping` | Stays in the reply and keeps its turn anchor |
| `Running` with a `turn_started_at` other than the anchor, whose `AgentState::prompt` is wake notices alone | Re-anchors on the new stamp and keeps waiting |
| `Running` with a new `turn_started_at` and any other prompt | Ends: a human or peer prompt, or a batch mixing a wake with another section, opened an unrelated turn |

`store::message::prompt_is_wake_only` decides the wake-only case: every section must be a `WAIT` or `SIGNAL`. The re-anchor holds even when no poll saw `Sleeping`.

`TurnWaitView::wake_in_flight(agent, digest)` owns the non-terminal harness-message test. The view captures one clock beside its queue read and excludes a `Sent` record past its hold deadline, deferrals included, without reconciliation. Reply waits pass `false`, counting `Wait` and `Signal` only. The supervised-run owed predicate passes `true` to also count `SubagentReport`. A digest alone does not hold a reply wait open.

One 500 ms poll reads the message list and cached snapshot once per tick and advances every unfinished leg. On entry and every tenth tick (`WAIT_GUARD_TICKS`) it also folds agent-context sidecars and re-checks for cycles.

### Cycle detection

Two agents that each wait on the other would hang forever, so [`wait_cycle`](../../../crates/rimz/src/message/reply.rs) builds a wait graph and refuses to enter a cycle. An edge runs from a sender card to a receiver card in two cases:

- A live `reply_wait` record that is `Queued`, `Claimed`, or `Sent`, from a named `Running` sender.
- A `Delivered` `reply_wait` record from history, when the receiver is `Running`, its `turn_opened_by` context lists the record, and the sender is named and `Running`.

Dispatch checks the graph before enqueue, and the periodic poll catches simultaneous dispatches and cycles that form later. The youngest `msg_` id in a detected cycle yields: only that reply leg fails, and its record is left untouched so the turn boundary still delivers the text. Older waits continue. Diagnostics name the blocking handle and message id and render the multi-hop chain.

### Settlement

The join succeeds only when every leg completes. Otherwise it takes the first non-completed status in target order. `--any` returns the first terminal leg without canceling the rest.

One deadline spans fan-out dispatch and every reply turn. A human's bare `--wait` is indefinite, an agent's defaults to one hour, and an explicit duration wins. On expiry every unfinished `Sent` record is marked timed out, every unfinished leg is classified `TimedOut`, and the command exits 124.

Before polling, the CLI prints each ordinary delivery receipt on stderr, in JSON mode too. Text output streams labeled replies in completion order, and `--json` buffers one handle-keyed map. A single agent that writes no assistant message yields empty stdout and a note on stderr. Each `ReplyResult` carries its leg's last observed message status, so deadline hints describe the same queued, sent, or delivered state that timeout settlement acted on, without re-reading the store. The timeout header and per-leg hints go to stderr in both output modes.

## Scheduling and wakeups

The room's sidebar host notices when a parked message comes due, through a deliberately thin handoff:

1. The store refreshes `lanes/message-wake.json` in its paired runtime after every live-queue file write, under the same workspace lock, from the records just written. The stamp is the earliest time worth a look: an ordinary-lane `not_before`, a `Queued` retry floor or ready-head backstop, a queued expiry or future registration deadline, a `Claimed` lease expiry, or an unconfirmed `Sent` reconcile deadline (30 s for prompts, 3 minutes for commands). Deliverable ordinary queued followers contribute only their expiry and a future `register_by`; scheduled or condition-pending records keep their own wake deadlines. Queued `Resume` records arm nothing. A standalone compaction left `Queued` arms the stamp too, so its sweep does not depend on later message traffic.
2. The host reads only that file, and when the stamp comes due spawns a detached `rimz --mux <mux> message sweep --workspace-id <id>` ([`fire.rs`](../../../crates/rimz/src/message/fire.rs)). For an unchanged stamp value it waits one prompt delivery window from the last spawn's cache-refresh pass time before spawning again; a changed due stamp wakes immediately. The host does no store reads, store writes, or queue selection.
3. The sweep finalizes expired automatic commands and unregistered resume follow-ups, reconciles stale `Sent` and expired `Claimed` records, evaluates unmet conditions, and delivers ready FIFO heads. It then calls `Store::rearm_message_wake` under the store's lock to refresh even an unchanged queue, since a queued record whose `not_before` passed contributes its retry floor instead. No-op queue commits alone do not refresh.

The stamp is a JSON `Option<Timestamp>`, atomically replaced without fsync, or removed when no record arms a wake. A refresh failure warns that the sweep may run late and never fails the queue commit. The refresh precedes history and event appends, so a later error cannot skip it. A workspace identity rewrite refreshes the moved room's paired runtime too. The sweep is single-flight through a `message-sweep.lock` file lock, so overlapping wakeups collapse into one pass.

A ready ordinary `Queued` head without a retry hint arms the stamp even without `not_before`, contributing its `updated_at`. That backstop recovers a message to an idle agent that missed the live send. Sending or archiving the head re-arms the next record's own deadline in that same queue commit. A queued perishable command caps its wake at its expiry, even when its schedule or retry floor is later. That only tightens cleanup latency, since read-time expiry already releases its FIFO slot. A queued resume follow-up caps its wake at a future `register_by`, so a later schedule cannot delay the registration check. A past registration deadline does not force repeated immediate wakes for a card that is already live.

Condition evaluation inside a sweep is one transaction. It evaluates every unmet condition against one context-enriched snapshot, then applies every stamp, retry floor, and watched-agent archive together. It reloads the pending records and delivers newly eligible heads from the same snapshot in the same run, unless their card was held in the queue read after reconciliation. A new stamp emits `message.after_met` or `message.when_met`. An unmet `when` condition sets `retry_after` to the exact projected trip time, so a 58-minute dwell wakes once at 58 minutes. A met stamp survives session end and receiver delay, which is how a busy receiver still gets the message at its next boundary.

### Back-off

The sweep backs off because the host ticks often. When it cannot deliver a ready head (gate closed, ask waiting, compacting, no pane, behind a `Sent` prompt, provider starting), it sets `retry_after` one delivery window ahead. A deliverable `Queued` non-`Resume` follower behind its card's head arms only its expiry and a future `register_by`, so it does not keep the stamp due, and the host retries an unchanged due stamp at most once per prompt delivery window. If a sweep dies or loses the single-flight lock without re-arming, that guard delays its next attempt by at most one window. Independent deadlines and changed due stamps still wake immediately. The deferred record stays `Queued` with its pane pin and its `attempts` untouched. On a `Queued` record, `retry_after` is only a wake hint: it does not affect readiness, FIFO position, claim leases, or hook-driven delivery.

A `NoPane` back-off also records the blocker in `last_error`, in the same queue commit and in the words `rimz message show` prints. A defer on any other verdict clears a `last_error` that a `NoPane` back-off wrote, so a pane that returns while the receiver is busy leaves the gate as the only thing `show` reports. It clears that sentence and nothing else: a real send failure recorded by a requeue stays. That is why the recognizer (`message::deliver::no_pane_blocker` and its matcher) sits beside the formatter, and the store applies it without reading the words. The next claim clears the error.

An ended receiver is terminal instead. The sweep archives open records for the card, plus records with unmet `when` conditions watching it, only when their `enqueued_at` is at or before the observed `ended_at`. The writers apply this cutoff under the workspace lock, so a concurrent resume's newer messages survive a stale sweep snapshot. The end-hook reactor archives without the cutoff, in realtime, and orphan GC is the backstop. The reason is `receiver ended; rimz message @<handle> resumes it` for a launched child, or `receiver ended` otherwise. A record archived for a watched session carries the condition in `last_error`.

### Schedules

Durations accept `s`, `m`, `h`, and `d`, and zero is rejected. Wall-clock `HH:MM` resolves to its next occurrence in the configured `timezone` (today if still ahead, else tomorrow), falling back to the system zone.

`rimz message --schedule` and `rimz wait` feel alike and work differently. A schedule is a floor on a record that already exists: the text is written now and held until its time. A wait is a loop task that holds no message until its trigger fires, then dispatches one as an automated `Harness { notice: Wait }` send with fan-out disabled and no caller attribution. That is why a wait can wait on a signal or a command, and why its text appears in the queue only when it lands ([loops.md § Waits](./loops.md#waits)).

## Sender notices

An agent that sent a message gets one notice when the message ends undelivered and one when it stays queued too long. Harness senders never get either, so a notice cannot cause a notice. Neither notice is an owed wake, and `reply_wait` records get neither, because their blocked CLI already reports failures and timeout hints.

**Undelivered.** `crates/rimz/src/store/writer/queue.rs::QueueTxn::close` collects agent-sent, non-`reply_wait` endings in `Archived`, `TimedOut`, `Abandoned`, or `Expired`. `Store::commit_queue` resolves the live sender and queues a `MESSAGE_UNDELIVERED` notice and its `message.queued` event in the same commit as the ending. A closed record leaves the live queue once, so an ending cannot notify twice. The notice quotes the ending reason and attempt evidence, previews the text, and asks the sender to inspect the receiver before resending. `Errored` is left out: a send failure errors inside the sender's own `rimz message` call, which exits non-zero with the reason. A queued compaction refused as a repeat also ends `Errored` without a notice. Delivered and deliberately canceled messages do not qualify.

**Still queued.** The sweep classifies a refused head's verdict through the exhaustive `crates/rimz/src/message/deliver.rs::DeliveryVerdict::stall_class`:

| Class | Delay | Verdicts |
| --- | --- | --- |
| NotWorking | 5 min | `NoPane`, `ProviderStarting`, `ResumeUnrecovered`, `AskWaiting`, `ReceiverGone`, `ReceiverEnded` when not archived, and `GateClosed` with a status other than `Running` (absent status included) |
| Busy | 20 min | `GateClosed` with `Running`, and `Compacting` |
| No notice | none | `WaitingForRegistration`, `Scheduled`, `WaitingOnAfter`, `WaitingOnWhen`, `BehindFifo`, `Expired`, `Ready`, or a stop without a verdict |

Each qualifying pending record on the refused head's card is measured from its own `enqueued_at`, not just the head, and its own schedule and conditions must be met. An unconfirmed-send requeue keeps that clock, while `message requeue` creates a fresh message. `Store::queue_still_queued_notice` rechecks `Queued` and an absent `queued_notice_at` under the workspace lock, then stamps that field and queues `MESSAGE_QUEUED` with its audit event together, so a second call changes nothing. The original message stays queued, and the notice names `rimz message cancel <id>` as the withdrawal path. The [millisecond overrides](../../reference/cli/message.md#the-message-header) for the two delays use the same `env_ms` convention as the delivery window, and are independent of it.

`crates/rimz/src/store/message/sender_notice.rs::resolve_sender` picks the recipient: an unended card of the sender's kind. When the sender has `agent_id`, it must match the card's session id or launch id. Otherwise the recorded name must match, and among several live cards with that name (as team roles across lanes) the one in the sender's recorded channel wins. It never falls back from a stale id to a recycled name. With no live sender there is no notice, only the ending's own audit. A notice is an ordinary queued `Harness` prompt with the sender card's channel, a `Done` gate, no pane pin, and no `in_reply_to` causality.

The queue commit refreshes the wake stamp for the notices it queues, including endings outside the sweep (lifecycle hooks, GC, worktree creation or retirement), so no caller-side refresh or notice timer is needed. Notices write no assist record: the message record and its queued audit event are the durable account.

## Storage and audit

```text
records/messages/messages.jsonl        live Queued, Claimed, and Sent records
audit/messages/<bucket-start>.jsonl    terminal records, with text
log/events.log.jsonl                   message.* audit events, without text
```

The queue file holds only live records and is the truth. One queue transaction rewrites `messages.jsonl` when the live set changed, then appends terminal records to history, then appends the audit events. Every write holds the workspace lock. The queue rewrite uses temp-file-plus-rename and history uses the append helper ([store.md § Write classes](../store.md#write-classes)). The queue file is created lazily, so an empty workspace costs the hook path one missing-file stat, and a missing file reads as no live records.

History uses the shared epoch-aligned seven-day bucket rule from `disk/buckets.rs`, keyed by each record's `updated_at`. `list_history()` reads buckets newest first and stops at the newest 500 records (`HISTORY_READ_LIMIT` in `store/message/codec.rs`). `gc.transcript_retention` owns history retention together with the conversation transcript, defaulting to `forever`, and the audit age sweep and size cap exclude both directories. Duration retention expires files by mtime.

History is audit, so an append failure warns and leaves the queue transition standing. This order matters with mixed binaries: if a history file an older binary cannot parse could fail the transaction, the queue would keep a record its caller believes settled, and the next sweep would deliver it again. A crash between the queue rewrite and the appends costs the terminal text and its audit event, never queue state.

The store reads through `list()` (live), `list_history()` (terminal, with text), and `list_pending()` (`Queued` only). The parked resume monitor instead uses `store::message::read_status` for one id, under the workspace lock, across the live queue and all retained history, without opening or folding a store. It does not use the newest-500 history view, because an older `Delivered` follow-up must keep the reopen hold and not look missing. The monitor caches the result behind the queue file's stamp.

The event log carries twelve methods: `message.queued`, `message.edited`, `message.after_met`, `message.when_met`, `message.sent`, `message.delivered`, `message.timed_out`, `message.errored`, `message.canceled`, `message.abandoned`, `message.expired`, and `message.archived`. The parser reads the retired `message.removed` as a cancellation. Some transitions rewrite the queue silently, with no event: a claim, a retryable pre-send failure returning a claim to `Queued`, a reconciliation hold, and a sweep that only moves `retry_after` and its blocker. An explicit claim release (`release_message_claims`) does write `message.queued`.

The payload carries `message_id`, `address`, `kind`, `agent_id`, `agent_name`, `channel`, `gate`, `status`, `body`, `pane_id`, the `forced` flag, sender attribution, `text_len`, both counters, timestamps, the compaction baseline, and an optional `reason`. The reason is set on errors, abandons, cancels, archives, expiries, releases and requeues, timeouts, edits (the changed fields), condition stamps, and mixed-submit deliveries. **Message text never enters the event log**, which is why `message list` merges three sources: live records, history records with text, and terminal rows known only from events.

### Mixed binaries

A room can be shared by binaries of different versions, and four record changes have a compatibility edge:

- `Expired` is a status an older binary cannot decode. Once an expired record is in `audit/messages/`, older binaries fail the history reads behind `message list`, `message show`, `message requeue`, and `send_compact`'s settled lookup. The terminal commit removes the record from the live queue, so older queue transactions never meet the status. Event-log readers keep unknown methods through their catch-all.
- `queued_notice_at` is dropped by an older binary that rewrites the queue, so a later sweep by a newer binary can send a second still-queued notice. No status or event method marks this.
- `about_run` is dropped by an older binary that rewrites the queue, so a linked notice can then deliver even after its child run has ended. Records written without a link retain their existing JSON shape.
- Harness notice strings a binary does not know survive its queue rewrites as `HarnessNotice::Other`.

## The inbox verbs

Flags and rendering are in [cli/message.md](../../reference/cli/message.md). Underneath:

- `message list` and `message show` merge the three sources above. `list` shows only conversation records unless `--system` is passed, independently of `--all`, lane, status, or target, and JSON follows the same filter. Human output reports a hidden-system count when it is nonzero, scoped before the row limit, even when no visible rows remain. JSON omits the count. `show` reads every sender class and renders the [ordered check](#the-ordered-check), naming the first unmet condition. The rendered handle comes from the record's enqueue-time `address`, then the live snapshot, then `agent_name` plus channel, then `kind:agent_id`.
- `message edit` is the single compare-and-swap path for a queued record. It accepts only `Queued`, refuses `Claimed` as in flight, reports terminal records from history, applies the delivery changes, clears `retry_after` so the next sweep sees them, and appends `message.edited` naming the changed fields. Receiver, channel, card, sender, and pane pin are not editable: retargeting is cancel plus send.
- `message steer` pushes one queued record through now ([The delivery helper](#the-delivery-helper)).
- `message interrupt <id> [--force]` pushes one queued record through the paste-then-key path. Claimed and terminal records are refused as for steer.
- `message requeue` copies a terminal history record into a fresh `Queued` record with a new id. It keeps text, receiver, channel, sender, body, delivery settings, `in_reply_to`, and `about_run`, and clears condition stamps. A run-linked notice is canceled again if its run is still ended. A terminal row known only from events cannot be requeued, because its text was never stored there.
- `message cancel` settles named open records (`Queued` or `Claimed`). A `Sent` record is past canceling, since its text is in the pane. `message clear <target>` cancels every open record for one card, and a targetless `message clear` cancels the scoped lane. Both include system records hidden from the inbox, and `clear` prints the ids it canceled.

Two hidden helpers do the pipeline's background work, each spawned detached: `message deliver --message-id <id>` and `message sweep --workspace-id <id>`. The sweep opens the recorded room by id without recording it again. The id wins over `--root`, and a missing workspace record is an error. Without an id, it resolves the participant room and opens only an existing store. No room means ``no room at <project_root>: run `rimz start` there, or pass --root <room>; nothing to sweep``, a nonzero exit, and no state created.

## Channels

A channel is the cooperation lane inside one room. It is the group the sidebar draws, the `#channel` suffix an address takes, and the tab name RimZ recovers on rebirth.

### Where a lane comes from

Launch resolves one lane and stamps it into `RIMZ_CHANNEL`, the launch event, and the rollup. [`resolve_room_channel`](../../../crates/rimz/src/harness/spec.rs) takes the first that applies:

1. An explicit `--channel`.
2. The current directory's basename, whenever it differs from the project root. A RimZ-owned worktree is the common case.
3. `<dir>/<team>` for a named team launched at the room root.
4. No lane, for a bare directory room.

Read paths use the stamped lane, and fall back to the lane-path basename only for agents with no stamp: recorded `checkout` before `worktree_path`. Hooks resolve the git toplevel from the agent's own cwd at any depth, but do not replace a recorded checkout, so process-directory changes do not move its lane. A non-git agent whose lane path is the room root folds into the room's root lane.

Lane equality scopes target resolution, rendered handles, sidebar grouping, `agents list`, pane overlays, `message list`, transcripts, and recovery. Branch names are display metadata on the worktree card and never define a lane.

### The shared namespace

No lane has a record of its own. Worktree lanes take their durable truth from the `rimz-worktree.json` marker ([worktrees.md](./worktrees.md)), and explicit, team, and directory lanes derive from the launch identity stamped on each agent. Worktrees still list without agents, but an empty explicit lane has nothing to list or restore, and a rebirth restores agent tabs only. The sidebar is presence-driven, so a group appears only while a pane runs in that lane. `rimz gc` removes a leftover `records/channels.json` under the `records` class, and no code reads or writes that file.

Explicit lanes and RimZ-owned worktrees share one namespace, and `channel.rs` owns both halves of the rule:

- An explicit `--channel NAME` launch is refused for a malformed name and for a name a managed worktree owns (`channel::admit_launch`, a Git read with no store access).
- `rimz worktree new NAME`, and a launch that would create an explicitly named worktree `NAME`, are refused while a live agent holds the lane (`channel::admit_worktree_name`). A live holder is an agent row with no end stamp, liveness other than dead, the lane stamped, and a checkout that is not the worktree of that name. Launch admission checks the dashed name before checkout creation. An existing worktree is entered without this check, fresh replacement included. Resume, generated names, and default `pr-<N>` names are not checked.

### Addressing into a lane

The default channel a command resolves in, and its precedence, are [fleet.md § Resolving the channel](./fleet.md#resolving-the-channel). Two consequences belong to messaging. Bare `message clear` from an agent clears that agent's channel, even from the main checkout, because the agent uses its stored card channel when `RIMZ_CHANNEL` is unset. A human shell in a bare directory room has no lane and reaches the whole room, and `message list` treats that as the main-lane inbox, which `--all` widens.

`--worktree` and `--channel` are separate launch intents. A worktree launch creates or reuses a Git checkout, subject to the [named-worktree entry check](../../reference/cli/agents.md#relaunch-into-a-named-worktree), and `message --create` is such a launch. A named-channel launch stays in the room root and records only the lane. Inline `#design` and `--channel design` reconcile through the same target parser, so a mismatch fails before delivery.

## Timing knobs

Every message timing in one place. The environment overrides take milliseconds through `env_ms`, and exist for tests and diagnosis.

| Knob | Default | Override | Used by |
| --- | --- | --- | --- |
| Settle before re-check | 400 ms | `RIMZ_MESSAGE_SETTLE_MS` | [delivery helper](#the-delivery-helper) |
| Message interval | 1 s | `RIMZ_MESSAGE_INTERVAL_MS` | fan-out pacing, compact-then-prompt |
| Command submit delay | 1 s | `RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS` | [typed commands](#commands-are-typed) |
| Interrupt delay | 3 s | `RIMZ_MESSAGE_INTERRUPT_DELAY_MS` | [the interrupt key](#the-interrupt-key) |
| Interrupt confirmation | 5 s | `RIMZ_MESSAGE_INTERRUPT_CONFIRM_MS` | Codex stop confirmation |
| Prompt delivery window | 30 s | `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` | `Sent` prompt deadline, sweep back-off, provider-start bound (ten windows) |
| Command delivery window | 3 min | `RIMZ_MESSAGE_COMMAND_DELIVERY_WINDOW_MS` | `Sent` command deadline |
| Automatic command validity | 10 min | `RIMZ_MESSAGE_COMMAND_VALIDITY_MS` | [perishable commands](#compaction-commands) |
| Resume registration limit | 2 min | `RIMZ_MESSAGE_REGISTER_TIMEOUT_MS` | [`register_by`](#a-resuming-recipient) |
| Unconfirmed prompt requeues | 3 | `RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS` | [reconciliation](#reconciliation) |
| Still-queued notice, not working | 5 min | `RIMZ_MESSAGE_QUEUED_NOTICE_IDLE_MS` | [sender notices](#sender-notices) |
| Still-queued notice, busy | 20 min | `RIMZ_MESSAGE_QUEUED_NOTICE_BUSY_MS` | [sender notices](#sender-notices) |
| Claim lease (`CLAIM_TTL`) | 15 s | none | [claims](#claims-and-holds) |
| Claim cap (`MAX_DELIVERY_ATTEMPTS`) | 5 | none | `Abandoned` |
| Compaction window (`COMPACTING_WINDOW_SECS`) | 90 s | none | check 9 |
| Pane write lock wait | 30 s | none | [writing to the pane](#writing-to-the-pane) |
| Reply poll | 500 ms, guard every 10 ticks | none | [reply waits](#reply-waits) |

## Hazards

- **Queued text can land on a half-typed human draft.** Delivery gates on store state, never on focused-pane state or captured composer contents, because a pane read is latency, not truth.
- **Agent UIs can show dialogs no hook reports.** Delivery never captures the pane. A script that must inspect UI text captures before sending through the public `pane capture` primitive.
- **Multiplexer writes are best-effort.** A pane can vanish or reject input after the claim. The record stores the error and retries to the cap.
- **A pane write is not an acknowledgement.** Treat `Sent` as pending. Anything that assumes the agent read the text belongs after `Delivered`.

## See also

- [transcript.md](./transcript.md): the conversation log confirmed deliveries write, and asks and answers.
- [fleet.md § The address](./fleet.md#the-address): the address grammar dispatch resolves through.
- [loops.md § Waits](./loops.md#waits): the loop tasks that dispatch messages when a trigger fires.
- [subagents.md](./subagents.md): the digest producer behind the join guard, and the run enrollment the acknowledgment callback performs.
- [store.md](../store.md): the write classes and durability contract the queue follows.
