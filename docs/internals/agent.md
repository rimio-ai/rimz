# Agent state and liveness

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes, [hooks.md](./hooks.md) for the agent boundary — the trait, the channels, install, and the per-provider native mappings — and [transcript.md](./transcript.md) for how context enrichment is read from each provider.

This doc owns how a running agent is *modeled*: the rollup that folds lifecycle observations into one state per agent, the state machine, posture, liveness, and enrichment. The seam — [hooks.md](./hooks.md) *produces* an [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs); this doc folds it into one [`AgentState`](../../crates/rimz/src/feed.rs); [sidebar.md](./sidebar.md) projects that state into a row.

The observation is agent-agnostic by construction, so everything below is too: a new agent that emits well-formed observations gets the state machine, ranking, liveness, and jump for free.

## The rollup

`reduce_agent_states` folds the lifecycle events into one `AgentState` keyed by `(kind, agent_id)` — `agent_id` is the agent's session id, so two concurrent agents of the same kind never share a row. Each event is a *partial* update; how the reducer treats a field the event omits is the field's **lifetime**:

- **identity** — set once when the session registers, stable thereafter (`agent_id`, `kind`, `parent_agent_id`, `agent_pid`).
- **activity** — replaced by the latest event, and *clearing* it is meaningful — an idle agent has no `task` (`status`, `task`, `last_activity`).
- **carry-forward** — capability/enrichment that persists until a newer value arrives; a missing value never resets it (`permission_posture`, `model`, `effort`, `context_pct`, `prompt`).
- **live-derived** — never stored in the ledger, computed at snapshot time from the live pane or git (`pane`, `worktree_path`, `worktree_branch`). The pane knows its current cwd every tick, so these follow a `git checkout`; pinning them at registration is the branch-tracking bug (see [Liveness and presence](#liveness-and-presence)).

[`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs) and [`AgentState`](../../crates/rimz/src/feed.rs) are the field catalog; the lifetimes above are the rule those types do not state.

`model` is stored **canonicalized** — a trailing capability tag is stripped (`claude-opus-4-8[1m]` → `claude-opus-4-8`). The tag rides only the fresh-launch payload; later events carry the bare id, so without canonicalization the `model` carry-forward would flip `…[1m]` → `…` the first time a suffix-less event arrived. Canonicalizing at reduce time pins one stable label while the event log stays faithful to the raw payload.

### Instance identity and age

`last_activity` is always the agent's *own* latest event, never inherited from a previous instance of the same kind. When a payload carries no session id the reducer keys on the captured `runtime_owner` (pid + start token) rather than a shared anonymous bucket, so two unidentified instances never merge; a truly unkeyable event is dropped — better no row than one that lies about its age.

## The state machine

The reducer takes each observation's `status` verbatim; the adapter decides which native event maps to which status (the [hooks.md](./hooks.md) appendices). The five values, in ranking order — most attention-hungry first, so a working `running` agent settles *below* the calm `idle`/`success`:

- `waiting` — blocked on a human decision *(raises attention)*
- `failed` — the last turn errored *(raises attention)*
- `idle` — wired in, nothing in flight
- `success` — last turn completed cleanly
- `running` — actively working a task

The glyph, animation, and color for each are the canonical table in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape); this doc owns the transitions, not the painting.

```text
   (none) ──registers──► idle ──turn starts / subagent starts──► running
                          ▲                                        │
           next prompt    │   turn ends clean ──► success          │
           re-enters ─────┤   turn ends errored ──► failed  ◄──────┤
           running        │   subagent ends ──► idle (child)       │
                          └────────────────────────────────────────┘
   blocking ask pending while running ──► waiting (feed channel, not lifecycle)

   session ends / pid dead / pane reverted to shell ──► removed (no row)
```

A turn-end observation resolves the turn to `success`, or `failed` on an error signal — never back to `idle`. One exception keeps it `running`: a turn end the adapter recognises as the main thread *parking on still-in-flight background work* is not a turn end, so the row stays `running` and labels itself with that work rather than painting a false `✓` (the provider-specific detection lives in [hooks.md → Appendix Claude](./hooks.md#appendix--claude-code)). An error still wins.

`waiting` is **not** a lifecycle transition — it is a pending blocking feed item joined to the agent (the feed channel; see [hooks.md → Two hook channels](./hooks.md#two-hook-channels)). The lifecycle channel drives the other four.

The displayed cell refines `running` two ways without changing the rollup: a `running` agent whose permission slider is `plan` renders as **thinking**, and one silent past the stall window escalates to the attention `!` (see [Liveness and presence](#liveness-and-presence)).

### Plan mode as a sticky posture

`plan` is one position of the permission slider, not a separate flag: thinking is `running` joined to `permission_posture == plan`. Rimz samples posture from lifecycle observations only (`posture_from_payload`); an observation that names no slider carries the prior value forward. It never infers plan mode from prompt text or transcript content.

- **Approving a plan** moves the slider off `plan`; the next observation that reports the new posture drops the thinking state.
- **Shift-tabbing out of `plan`** mid-turn may raise no observation, so the sidebar can lag until the next one. That bounded latency is intentional — the sidebar is observational, and the simpler model avoids transcript/prompt heuristics.
- **Subagents** own separate `agent_id`s, so a child observation never mutates its parent's posture.

The agent owns status and posture; Rimz observes and renders. `yolo` is read from the agent's own bypass flag; `interactive` folds into `default`. The vocabulary is defined once in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape).

## Enrichment is display-only

`task`, `context_pct`, `total_tokens`, and the todo counts are **enrichment**: display-only, redactable, and they never drive routing, ranking, or a decision (the no-transcript-correctness rule). A missing value means "the agent didn't report it," never zero — the sidebar projects it to a visible 0% baseline so every observed agent paints a context bar.

Context budget is the one field no agent puts in its hook JSON — usage lives in the transcript, captured from the **transcript tail** on the turn-boundary events Rimz already fires (discovery and the per-provider mapping are [transcript.md](./transcript.md)'s concern). These are bare token counts; `payload_mode` gates the *content* of high-frequency payloads, never these gauges.

### Rich context (`AgentContext`)

Some agents publish far richer per-session data out of band than their hooks carry — context-window token accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, PR info, version, effort. The *transport* differs per agent and lives in [transcript.md](./transcript.md) (Claude's statusline feed, Codex's app-server); `observe_context` normalizes any transport into the agent-agnostic [`AgentContext`](../../crates/rimz/src/agents/mod.rs). Every field is `Option` and tolerantly parsed, so a sparse or evolved payload always parses and the renderer draws whatever subset is present. The account/balance subset (plan, metered, the 5h/7d windows) is account-scoped and folds into the provider dashboard — its mapping and aggregation are [account.md](./account.md).

This is high-frequency, display-only enrichment, so it does **not** ride the event log. The feed process writes a **latest-wins per-session sidecar** — one atomic file per `(kind, agent_id)` under the runtime `agent_context/` dir — and `rimz sidebar snapshot` folds each record onto its `AgentState` (`with_agent_context`). The sidecar lives wholly off the durable path (ledger first — sidebar wakeups are latency, not truth) and dies with the session: a session-end event tombstones it, and a read past the ghost-session TTL drops it even if the tombstone was missed. It is not `payload_mode`-gated; when a loader lands, gate only the content-ish fields (PR url, output style, vim mode), never the numeric gauges. The file sits under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

## Liveness and presence

Presence comes from the live pane, not a session-exit hook: an agent renders only on the pane it stamped, and one whose pane reverts to a shell or closes is gone with no exit event required. **The binding mechanics — stamped pane id, the Codex daemon exception, jump reconciliation — live in [sidebar.md → Presence model](./sidebar.md#presence-model); this section owns only what the rollup contributes.** There is no `offline` status: a dead agent is a reverted shell row or no row, never a retracted ledger fact.

**Pid is a hygiene signal, not a render gate.** Stamped-pane binding already keeps a stale agent off a stranger's pane, so the captured pid never gates rendering. Rimz records it best-effort on each lifecycle event (`RIMZ_AGENT_PID=$PPID`, falling back to a `/proc` ancestor walk, plus the Linux process-start token to defeat pid reuse); the reaper reads *pidless* as one ghost signal.

**Per-tool activity is a heartbeat, not an event.** The durable event log is turn-grained, so `last_activity` would otherwise advance only at turn boundaries. The hook touches a per-agent heartbeat ([`agent_activity`](../../crates/rimz/src/agent_activity.rs)) on every progress-proving event — each completed tool call, the turn boundaries, subagent start/stop — and the snapshot folds the freshest touch into `last_activity`. It is *not* touched on a pre-tool event (which can fire in the same call as a blocking ask) or while blocked. This signal does three things: it keeps a busy agent's row animating (the spinner tracks real work, not a stale window), it escalates a `running` agent silent past the ~10-minute stall window to the `!` attention state, and it recovers an answered `native_ui` ask — once `last_activity` passes the ask the snapshot stops folding it, so an agent that answered in its own UI returns to `running` without waiting for the next turn boundary. Like every heartbeat it is latency, not truth: a missing file just leaves the event-log timestamp.

**The rollup reaps its own ghosts.** A session that never captured a pid and never fired a session-end event would pin a stale row forever, and relaunch-in-place or shared-pid sessions stack duplicates. At snapshot time the derived rollup (never the event log) drops two classes, both safe under one-pane-one-row: a *pidless* session past a few-hours TTL, and an *older* same-kind session on the same `(worktree_path, worktree_branch)` superseded by a strictly-newer one when the older holds no live pane the newer doesn't already occupy. An agent holding its own distinct pane is always kept. The rules are **root-only** — a paneless, pidless subagent is never reaped on its own and children leave the rollup only when their parent does. This is workspace-local and complements the cross-workspace `rimz gc`.
