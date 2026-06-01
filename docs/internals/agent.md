# Agent state and liveness

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes, [hooks.md](./hooks.md) for the agent boundary — the trait, the channels, install, and the per-provider native mappings — and [transcript.md](./transcript.md) for how context enrichment is read from each provider.

This doc owns how a running agent is *modeled and rendered*: the rollup that folds lifecycle observations into one state per agent, the state machine, presence, liveness, and enrichment. The provider boundary — how a native protocol becomes an observation — is [hooks.md](./hooks.md). The seam between them is the [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs): hooks.md *produces* it; this doc folds it into one [`AgentState`](../../crates/rimz/src/feed.rs); [sidebar.md](./sidebar.md) projects it.

The observation is agent-agnostic by construction, so everything below is too. A new agent that emits well-formed observations gets the state machine, ranking, liveness, and jump for free.

## The unified global state

`reduce_agent_states` folds the lifecycle observations into one `AgentState` keyed by `(kind, agent_id)`. Each observation is a *partial* update: `status` always comes from the event, capability fields carry forward, and activity fields are replaced. The result is the agent row the sidebar projects.

### Attribute catalog

Each field, the [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs) field it folds from, and its **lifetime** — the rule the reducer follows when an event omits it:

- **identity** — established once when the session registers, stable for the session.
- **activity** — replaced by the latest event; clearing it is meaningful (an idle agent has no task).
- **carry-forward** — capability/enrichment that persists until a newer value arrives; a missing value never resets it.
- **live-derived** — not stored in the ledger; computed at snapshot time from the live pane list or git (see [sidebar.md → Presence model](./sidebar.md#presence-model)).

| Field                               | Meaning                                                      | Observation field                             | Lifetime      |
| ----------------------------------- | ------------------------------------------------------------ | --------------------------------------------- | ------------- |
| `agent_id`                          | session/instance key                                         | `agent_id`                                    | identity      |
| `parent_agent_id`                   | root session of a subagent                                   | `parent_agent_id`                             | identity      |
| `kind`                              | `claude` / `codex`                                           | the ingest source                             | identity      |
| `status`                            | 5-value rollup (below)                                       | `status`                                      | activity      |
| `permission_posture`                | `default`/`plan`/`auto`/`yolo`/`unknown` (`plan` → thinking) | `permission_posture`; missing carries forward | carry-forward |
| `task`                              | what it's working on                                         | `task`                                        | activity      |
| `prompt`                            | latest user prompt (line-2 label past idle)                  | `prompt`                                      | carry-forward |
| `model`                             | `Opus`, `GPT-5.5`                                            | `model` (canonicalized — below)               | carry-forward |
| `effort`                            | `xhigh`/`high`/…                                             | `effort`                                      | carry-forward |
| `context_pct`                       | context-window % gauge                                       | `context_pct`                                 | carry-forward |
| `total_tokens`                      | cumulative tokens                                            | `total_tokens`                                | carry-forward |
| `todo_done` / `todo_total`          | plan progress dots                                           | `todo_done` / `todo_total`                    | carry-forward |
| `agent_pid` / `agent_process_start` | liveness hygiene gate                                        | `agent_pid` / `agent_process_start`           | identity      |
| `runtime_owner`                     | owner-process identity                                       | `runtime_owner`                               | identity      |
| `worktree_path` / `worktree_branch` | grouping spine                                               | live pane cwd (ledger value is fallback)      | live-derived  |
| `pane`                              | jump target                                                  | bound live from the pane list                 | live-derived  |
| `last_activity`                     | age + attention rank                                         | `event.timestamp` + activity heartbeat        | activity      |
| `last_seen`                         | carryover-merge tiebreak                                     | `event.timestamp`                             | activity      |

The catalog turns on one distinction: **identity vs. live-derived**. `worktree_*` and `pane` are *live* facts — the pane knows its current cwd every tick — so they are derived at snapshot time, not pinned when the session registers. Pinning them is the branch-tracking bug (§ Liveness and presence).

The reducer stores `model` **canonicalized** — a trailing capability tag is stripped (`claude-opus-4-8[1m]` → `claude-opus-4-8`). The tag rides only on a fresh-launch payload; later events carry a new `agent_id`, the transcript records the bare id, and no model env var exposes the tag. So a suffix-less follow-up plus the `model` carry-forward would flip the label `…[1m]` → `…` the first time it arrived. Canonicalizing at reduce time pins one stable id while the event log stays faithful to the raw payload.

### The state machine

The reducer takes each observation's `status` verbatim; the adapter decides which native event maps to which status (see the [hooks.md](./hooks.md) appendices). The five-value set, in ranking order (most attention-hungry first — a working `running` agent is the least, so it sorts below the calm-but-settled `idle`/`success`), per [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape), which owns the full glyph/animation/color table:

| Status    | Glyph | Meaning                     | Raises attention |
| --------- | ----- | --------------------------- | ---------------- |
| `waiting` | `?`   | blocked on a human decision | yes              |
| `failed`  | `!`   | the last turn errored       | yes              |
| `idle`    | `○`   | wired in, nothing in flight | no               |
| `success` | `✓`   | last turn completed cleanly | no               |
| `running` | `⢿`   | actively working a task     | no               |

The displayed cell refines `running` two ways without changing the rollup: a `running` agent whose permission slider is in `plan` renders as **thinking** (`✽`, a sparkle animation — the `plan` posture below), and a `running` agent silent past the stall window escalates to the attention **`!`** (see [Liveness and presence](#liveness-and-presence)). A working `running` agent animates a braille spinner; the resolver-mid-flight overlay animates a braille spinner. Only these active states animate — `?`, `!`, `○`, `✓` are static so attention stays scannable.

`waiting` is **not** a lifecycle transition. It is a pending blocking feed item joined to the agent (the feed channel — see [hooks.md → Two hook channels](./hooks.md#two-hook-channels)). The lifecycle channel drives the other four:

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

A turn-end observation resolves the turn — `success`, or `failed` on an error signal — never back to `idle`. One adapter exception keeps it `running`: a turn end the adapter recognises as the main thread *parking on still-in-flight background work* is not a turn end, so the row stays `running` and labels itself with that work rather than painting a false `success` (the provider-specific detection lives in [hooks.md → Appendix Claude](./hooks.md#appendix--claude-code)). An error still wins. `idle` is the resting state a freshly-registered session establishes and a finished subagent child returns to; `success`/`failed`/`idle` all re-enter `running` on the next prompt.

The agent owns status and posture; Rimz observes and renders. `yolo` is observed from the agent's own bypass flag; `plan` is a first-class read-only posture (rendered as thinking while running); `interactive` folds into `default`. The vocabulary is defined once in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape).

#### Plan mode as a sticky posture

`plan` is one position of the permission slider, not a separate flag: `thinking` is `running` joined to `permission_posture == plan`, and the sparkle paints only while the agent is `running`. Rimz samples the posture from lifecycle observations only (`posture_from_payload`); an observation that names no slider carries the prior value forward. Rimz does not infer plan mode from prompt text or transcript content.

- **Approving a plan** moves the slider off `plan`, so the next lifecycle observation that reports the new posture drops the sparkle.
- **Shift-tabbing out of `plan`** mid-turn may raise no lifecycle observation. The sidebar may therefore lag until the next one; that bounded display latency is intentional, because the sidebar is observational and the simpler state model avoids transcript or prompt heuristics.
- **Subagents** own separate `agent_id`s, so a child observation never mutates its parent's posture.

### Instance identity and age

An agent row belongs to **one running instance**. The key is `(kind, agent_id)` with `agent_id` the agent's session id, so two concurrent agents of the same kind never share a row and `last_activity` is always the agent's *own* latest event — never inherited from a previous instance of the same kind.

When a payload carries no session id, the adapter keys on the captured `runtime_owner` (pid + start token) rather than a shared anonymous bucket, so two unidentified instances never merge; a truly unkeyable event is dropped rather than collapsed — better no row than a row that lies about its age.

### Enrichment is display-only

`task`, `context_pct`, `total_tokens`, and the todo counts are **enrichment**: display-only, redactable, and they never drive routing, ranking, or a decision (the no-transcript-correctness rule). The reduced agent state keeps missing context as "the agent didn't report it"; the sidebar row projects that missing value to a visible 0% baseline so every observed agent has a context bar.

Context budget is the one field no agent puts directly in its hook JSON — usage lives in the transcript, captured from the **transcript tail** only after a payload supplies a session id, on the turn-boundary lifecycle events Rimz already fires (discovery and the per-provider mapping are [transcript.md](./transcript.md)'s concern). These are bare token counts (metadata); `payload_mode` gates the *content* of high-frequency event payloads, never these gauges or the *state transition* they ride on.

### Rich context (`AgentContext`)

Some agents publish far richer per-session data out of band than their hooks carry — context-window token accounting and the most-recent message's usage breakdown, cost, rate-limit windows with reset instants, model display name, PR info, version, effort, and more. Each agent's *transport* differs and lives in [transcript.md](./transcript.md) (Claude's statusline feed, Codex's app-server); `observe_context` normalizes any such transport into the agent-agnostic [`AgentContext`](../../crates/rimz/src/agents/mod.rs). The account and balance subset (plan, metered, the 5h/7d windows) is account-scoped and folds into the provider dashboard — its mapping and aggregation are [account.md](./account.md). Every field is `Option` and tolerantly parsed, so a sparse or evolved payload always parses and the renderer draws whatever subset is present.

This is high-frequency, display-only enrichment, so it does **not** ride the event log. The feed process writes a **latest-wins per-session sidecar** — one atomic file per `(kind, agent_id)` under the runtime `agent_context/` dir — and `rimz sidebar snapshot` folds each record onto its `AgentState` by session key (`with_agent_context`). The sidecar lives wholly off the durable path: routing correctness stays in the ledger ("Ledger first — sidebar wakeups are latency, not truth"). It dies with the session — a session-end event tombstones it, and a read past the ghost-session TTL drops it even if that tombstone was missed.

`AgentContext` is metadata and gauges of the same class as `context_pct`/`total_tokens`, so it is not gated by `payload_mode`. When a `payload_mode` loader lands, gate only the content-ish fields (PR url, output style, vim mode); the numeric gauges, cost, and rate-limit windows stay always-on. The sidecar lives under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

## Liveness and presence

Presence comes from the live pane list, not from a session-exit hook (see [sidebar.md → Presence model](./sidebar.md#presence-model)). The binding is exact: every lifecycle event stamps the mux's own per-pane env var (`TMUX_PANE` / `ZELLIJ_PANE_ID`) — the mux's ground-truth pane assignment, not an agent self-claim — and the snapshot binds each live pane to the one agent that stamped that exact id. An agent renders only on its stamped pane; one whose pane reverts to a shell, closes, or is otherwise absent from the live list is gone, with no exit event required.

The captured pid is **not** a render gate — stamped-pane binding already keeps a stale agent off a stranger's pane. It feeds the rollup's hygiene instead: on a lifecycle event Rimz records the agent's pid best-effort (`RIMZ_AGENT_PID=$PPID`, falling back to a `/proc` ancestor walk, plus the Linux process-start token to defeat pid reuse), and the reaper below reads *pidless* as one of its ghost signals.

There is no `offline` status — a dead agent is a reverted shell row or no row at all, never a retracted ledger fact.

**Per-tool activity is a heartbeat, not an event.** The durable event log is turn-grained — `last_activity` would otherwise advance only at turn boundaries — so the hook touches a per-agent activity heartbeat (`runtime/agent-activity/`, the [`agent_activity`](../../crates/rimz/src/agent_activity.rs) module) on every progress-proving event (each completed tool call, the turn boundaries, subagent start/stop), and the snapshot folds the freshest touch into `last_activity`. It is **not** touched on a pre-tool event (which can fire in the same tool call as a blocking ask) or while the agent is blocked. This per-tool signal does three things: it keeps a busy agent's row animating (the spinner tracks real work, not a stale window), it escalates a `running` agent silent past the ~10-minute stall window to the `!` attention state, and it recovers an answered `native_ui` ask — the snapshot stops folding an ask onto the row once `last_activity` passes the ask, so an agent that answered in its own UI and kept working returns to `running` without waiting for the next turn boundary. Like every heartbeat, it is latency, not truth: a missing or stale file just leaves the event-log timestamp.

**The rollup reaps its own ghosts.** A session that never captured a pid and never fired a session-end event would otherwise pin a stale row forever, and relaunch-in-place or shared-pid sessions stack duplicates. At snapshot time the derived rollup (never the event log) drops two classes, both safe for one-pane-one-row: a *pidless* session past a few-hours TTL, and an *older* session superseded by a strictly-newer same-kind session on the same `(worktree_path, worktree_branch)` when the older holds no live pane the newer doesn't already occupy. An agent holding its own distinct pane is always kept. This is workspace-local and complements the cross-workspace `rimz gc`.

Two consequences this contract enforces (status in [Implementation status](#implementation-status)):

- **A pane the agent never stamped is a process row.** Command and cwd never bind a row, so after an agent exits and `git log` (or a fresh `node`) runs in the same or a neighbouring pane, that pane has no agent that stamped it and stays a process row. Two same-kind agents in one worktree — indistinguishable by command and cwd — bind only to their distinct stamped panes and never cross-wire. The lone exception is a pane-less Codex agent fired by the app-server daemon, which binds the live `codex` pane in its worktree by cwd ([Implementation status](#implementation-status), item 6).
- **Worktree and branch track the live pane.** Branch and worktree are resolved from the pane's current cwd at snapshot time (the same place diff stats are read), so they follow `git checkout` and a pane `cd` into another worktree. The ledger's pinned `worktree_*` is a fallback only for a detached agent with no live pane.

Pane binding and jump are the snapshot's job, documented in [sidebar.md → Jump](./sidebar.md#jump--the-row-is-the-link): binding is by the stamped pane id alone — no command or cwd fallback, save the Codex daemon's pane-less cwd bind ([Implementation status](#implementation-status), item 6) — and every jump reconciles pane id *and* `pane_process_start` so a reused id never focuses a stranger.

## Implementation status

The contract above is implemented. The history below is kept so the rationale for each fix stays discoverable.

1. **A registered session reaches `running` and carries its task.** The turn-start observation moves the agent to `running` and folds in the prompt; the turn-end observation resolves it. Both adapters wire the turn-start and turn-end events.
2. **Turn ends map to `success`/`failed`** via a shared `stop_status_from_payload` — a clean completion is `success`, an explicit error signal is `failed`. `idle` is owned by session-register and subagent-stop, never a turn-end outcome.
3. **Context budget is captured** from the transcript tail on the low-frequency turn-boundary events (the read-path is [transcript.md](./transcript.md)).
4. **Agent visibility no longer requires a pid.** `RuntimeScope::Runtime` applies the owner-required filter to `Surface::Script` items only; agents and bridge asks are kept unless a known owner is known-dead, so a pid-less agent still renders — on its stamped live pane.
5. **The branch label is re-derived live** from each worktree group's path by the snapshot CLI (cached under the diff-stats TTL), so the header follows a `git checkout`; the pinned ledger branch is the fallback when no live worktree resolves.
6. **Pane binding is by the stamped pane id alone, with one daemon exception.** Each lifecycle event stamps `TMUX_PANE`/`ZELLIJ_PANE_ID`, and the snapshot binds a live pane to the one agent that stamped it — one pane, one row, by construction. The earlier command/launcher heuristics (the `node`/`bun`/`deno`/`python` loose match) and their planned pid-ancestry refinement are removed as unnecessary: a pane the agent never stamped is a process row, full stop. The exception is Codex under remote control: its hooks fire from the per-user app-server daemon (see [transcript.md → Appendix Codex](./transcript.md#appendix--codex)), which runs detached with the session's cwd but no pane env, so the agent is pane-less by construction. As a last resort — only after the stamped-id and host checks miss — a pane-less Codex agent binds the live `codex` pane whose cwd equals its worktree (`codex_for_pane`); two in one worktree resolve most-recently-active, and the rollup reaper collapses the stale one. The match is exact-worktree and Codex-only: a parent checkout never captures a nested worktree's pane, and a pane-less Claude agent — always stamped while live, hence genuinely gone — is never rescued. A never-prompted Codex has no agent state yet (Codex registers its session lazily on the first prompt); when its hooks are wired, `codex_for_pane` synthesizes an idle `○ codex` row for the live pane until that first turn binds the real session — but an *unwired* Codex stays a process row, since Rimz can report no status for it (agents are invisible until their hooks are wired).
7. **A turn parked on background work stays `running`.** When the main thread spawns background tasks/agents and parks, the adapter detects the still-in-flight work, upgrades the clean turn end to `running`, and labels the row with that work, so the sidebar no longer paints `✓` on a busy agent. The provider-specific detection lives in [hooks.md](./hooks.md#appendix--claude-code).
