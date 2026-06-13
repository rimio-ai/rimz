# Agent Messages

Rimz accepts human-authored text for a live agent now (`rimz steer`) or for the next open delivery point (`rimz queue`). The feature uses the same pane-send primitive as humans and resolvers, while state decisions come from the ledger snapshot and hook lifecycle.

## Targets

Message commands resolve `TARGET` with the same card ref grammar as [`rimz agents`](../../reference/cli/agents.md): a normalized pane id (`tmux:%1`, `zellij:terminal_3`), exact pet name, kind ordinal (`claude-2`), unique kind, or agent session id/prefix. Add `@<worktree>` inline or pass `--worktree` to narrow matches by branch, path basename, or full path. Ambiguous matches and misses fail with candidate `(name, kind ordinal, worktree, pane)` labels.

## Steer

`rimz steer <target> -- <text>` types into the resolved agent's bound pane immediately and appends Enter by default. A pending feed ask attached to that agent blocks the send; `--force` records the override. The `agent.steered` event records kind, session id, pane id, force flag, and text length. Message content stays out of the event log.

## Queue Layout

Queued messages live under the workspace state root:

```text
queue/<msg_id>.json
queue/terminal/<msg_id>.json
```

`msg_` ids are UUIDv7, so filename order is FIFO order. Pending scans read only `queue/*.json`; claimed and final records move atomically into `queue/terminal/`. The directory is created lazily, so a workspace with no queued messages costs the hook path one missing-dir stat.

Each record stores the workspace id, agent kind, agent session id, text, Enter flag, delivery gate, status, enqueue/update timestamps, attempt count, last attempt timestamp, last error, and delivered timestamp. Status values are `pending`, `claimed`, `delivered`, `removed`, and `abandoned`.

## Gates

`--on done` opens when the rollup status is `idle` or `success`. `--on any` also opens on `failed`. `running`, `waiting`, and `paused` keep delivery closed. A pending ask attached to the agent keeps delivery closed for every gate, because the next input belongs to that ask.

The queue requires installed and trusted hooks for the target agent. Hooks are the delivery signal; accepting a queue entry for an unwired agent would create durable work with no transition that can release it.

## Delivery

Only unparked root turn ends trigger delivery. `Registered`, subagent stops, compaction events, and parked background turn ends do not check the queue. The lifecycle hook records the event, then spawns a detached `rimz queue deliver --message-id <id>` helper with nulled stdio for the FIFO head.

The helper waits `400ms` by default (`RIMZ_QUEUE_SETTLE_MS` overrides this for tests), reads the pending head, checks a fresh snapshot for the gate, pending-ask predicate, and bound pane, then claims the head under the workspace lock immediately before sending. State misses leave the message pending for a later transition. The claim moves the record to `claimed`, outside the pending scan, and increments the attempt count. A successful send moves the record to `delivered`; a send failure records `last_error` and returns it to `pending`, and after five attempts the record becomes `abandoned`.

The claim timestamp throttles retries after a send failure. A crash after claim leaves a visible `claimed` record that `queue list` surfaces; it is not auto-redelivered on a later turn end.

## Audit Events

Queue writes append `message.queued`, `message.delivered`, `message.removed`, and `message.abandoned` events. Events include message id, kind, agent id, gate, status, text length, Enter flag, attempt count, and reason. They never include message text.

`rimz gc` abandons open messages whose `(kind, agent_id)` no longer appears in the current rollup. This is maintenance, not delivery; normal state misses stay pending.

## Hazards

Queued text can still land while a human has half-typed a draft in the agent pane. Rimz gates on ledger state, not focused-pane state or captured composer contents.

Agent UIs can present dialogs that are not represented as feed asks. Core keeps pane capture out of message delivery; resolvers that need to inspect UI text own capture-before-send.

Multiplexer sends are best-effort. A pane can disappear or reject input after the claim. The queue records the error and retries on future turn-end transitions until the attempt cap.
