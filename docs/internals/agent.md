# Agent state and liveness

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes, [hooks.md](./hooks.md) for the agent boundary — the trait, the channels, install, and the per-provider native mappings — and [transcript.md](./transcript.md) for how context enrichment is read from each provider.

This doc owns how a running agent is *modeled*: the rollup that folds lifecycle observations into one state per agent, the state machine, posture, liveness, and enrichment. The seam — [hooks.md](./hooks.md) *produces* an [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs); this doc folds it into one [`AgentState`](../../crates/rimz/src/feed.rs); [sidebar.md](./sidebar.md) projects that state into a row.

The observation is agent-agnostic by construction, so everything below is too: a new agent that emits well-formed observations gets the state machine, ranking, liveness, and jump for free.

## The instance lifecycle

An agent reaches the sidebar as an **agent instance** — a live local pane running a known agent (`command_agent_kind`), bound one-to-one to its pane id, `pane_pid`, and process-start (derived from the in-pane CLI's `/proc` entry when the backend reports none natively, as Zellij does). A **session** (the hook-reported id the rollup keys on, `(kind, agent_id)`) binds to it, and the instance exits when its pane reverts to a shell. The instance exists before any session id is known, and the lifecycle's one hard problem is joining the two — which turns on two independent axes: how the hook reports its identity, and where the agent runs.

**Hook identity — stamped or daemon-routed.** A **standalone** agent runs in its pane, so the hook is a descendant of it and reads the pane's `ZELLIJ_PANE_ID` / `TMUX_PANE` and pid directly: it **stamps** the pane id onto the session. A **daemon-routed** agent runs through a background daemon, so the hook fires from the *daemon* — no pane env, the *daemon's* pid (shared by every client) — and the session is **unstamped** (no pane id). Claude is always standalone; Codex is standalone with no daemon, and daemon-routed under `codex remote-control start` (a per-user singleton).

**Presence — in-pane or remote.** Orthogonal to hook identity is where the agent actually runs:

- **In-pane.** A local pane runs the agent, with its own client `pane_pid`, so the user can jump to it. A standalone agent binds its pane by the stamped id; a daemon-routed *in-pane* agent (a Codex CLI thin-wrapping the daemon) is unstamped, so it binds the live `codex` pane by cwd instead. Either way it renders as a normal, jump-able agent row.
- **Remote.** The agent runs only in the daemon, with no local pane — `claude remote-control --spawn worktree`, or a Codex thread started from the web. It carries a worktree but no `pane_pid` and nothing to focus. **Rimz does not render remote agents yet — a documented gap, deferred to a future round ([sidebar.md → Presence model](./sidebar.md#presence-model)).** The `claude remote-control` host pane itself is separate infrastructure, filtered out of the room ([`pane_is_host`](../../crates/rimz/src/remote_control.rs)) and surfaced as the `⇅ rc` flag.

So the binding test is one question: does a live local pane bind the session? A stamped session binds by id, an unstamped one binds by cwd, and a session no pane binds is a remote agent.

**Phase 1 — pre-session presence (instance idle).** A wired agent instance with no bound session yet renders as an idle agent row, so a just-launched agent reads as itself rather than a bare process. Claude reaches this through a real `SessionStart` at launch, so its instance and session coincide immediately. An agent that [registers its session lazily](../../crates/rimz/src/agents/mod.rs) — Codex fires no `SessionStart` until the first prompt ([hooks.md → Appendix Codex](./hooks.md#appendix--codex)) — has an instance on screen before any session, so Rimz synthesizes an idle `○ <kind>` row for it until the first turn binds the real session ([`idle_agent_row`](../../crates/rimz/src/ledger/snapshot.rs), gated on the kind being wired). The capability is the agent's, not a special case: `registers_session_lazily` opts an agent into the synthesis and the cwd-bind, so a new lazy agent slots in without bespoke sidebar code, while Claude (always stamped) keeps the default and stays unaffected. The general rule: a wired instance with no session is an idle agent; an unwired one stays a [process row](./sidebar.md#process-rows).

**Phase 2 — session binding.** A lifecycle hook arrives carrying a session id, and Rimz joins it to the right instance. A standalone hook stamped the pane id, so the join is exact and free. A daemon-routed hook stamped no pane, so the unstamped session falls to a fallback ladder — exact-cwd today, gated by process-start so a freshly-started `codex` (whose start postdates a prior completed session in the same cwd) is refused rather than inheriting it, with the multi-client-one-cwd case unsolved. The ladder and its limits are [sidebar.md → Presence model](./sidebar.md#presence-model).

**Phase 3 — instance exit.** The in-pane agent process is the liveness truth, surfaced through the pane: the CLI client is the pane's foreground process, so when it exits the pane reverts to a shell and stops reading as an agent — the instance leaves with no exit hook, in both launch modes. A `SessionEnd` hook (Claude) tombstones the session eagerly on top of this, so its pending asks and context sidecar clear at once; Codex has no `SessionEnd`, so a Codex session leaves by pane liveness and the [rollup reaper](#liveness-and-presence) alone. One residue is daemon-routed-only: an unstamped session whose in-pane CLI has exited still sits in the rollup, and its recorded `agent_pid` is the shared daemon, which outlives the client — so the reaper's pid hygiene cannot clear that unstamped remnant once the client's pane is gone. The app-server's loaded-thread set is the signal that does: the producer reaps a daemon-mode session absent from `thread/loaded/list` before the pane fold ([`drop_dead_daemon_sessions`](../../crates/rimz/src/ledger/snapshot.rs)), so its stale stats never reach a live pane, while an unreachable daemon or an untrusted list keeps every session ([sidebar.md → Presence model](./sidebar.md#presence-model)).

## The rollup

`reduce_agent_states` folds the lifecycle events into one `AgentState` keyed by `(kind, agent_id)` — `agent_id` is the agent's session id, so two concurrent agents of the same kind never share a row. Each event is a *partial* update; how the reducer treats a field the event omits is the field's **lifetime**:

- **identity** — set once when the session registers, stable thereafter (`agent_id`, `kind`, `parent_agent_id`, `agent_pid`).
- **activity** — replaced by the latest event, and *clearing* it is meaningful — an idle agent has no `task` (`status`, `task`, `last_activity`). A subagent is the one exception: its `task` is its type (`Explore`, …) and carries forward as identity, so a finished child stays labeled when its `SubagentStop` omits the type.
- **carry-forward** — capability/enrichment that persists until a newer value arrives; a missing value never resets it (`permission_posture`, `model`, `effort`, `context_pct`, `prompt`).
- **live-derived** — never stored in the ledger, computed at snapshot time from the live pane or git (`pane`, `worktree_path`, `worktree_branch`). The pane knows its current cwd every tick, so these follow a `git checkout`; pinning them at registration is the branch-tracking bug (see [Liveness and presence](#liveness-and-presence)).

[`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs) and [`AgentState`](../../crates/rimz/src/feed.rs) are the field catalog; the lifetimes above are the rule those types do not state.

A compaction hook (`compacting: true`) is a fifth, transient lifetime: it stamps `compacting_since` and keeps the prior status (compaction is a head the sidebar paints, not a transition), and the next lifecycle event clears it. The projection also expires the marker past a short window, so a crash mid-compact can never pulse the head forever.

`model` is stored **canonicalized** — a trailing capability tag is stripped (`claude-opus-4-8[1m]` → `claude-opus-4-8`). The tag rides only the fresh-launch payload; later events carry the bare id, so without canonicalization the `model` carry-forward would flip `…[1m]` → `…` the first time a suffix-less event arrived. Canonicalizing at reduce time pins one stable label while the event log stays faithful to the raw payload.

### Instance identity and age

`last_activity` is always the agent's *own* latest event, never inherited from a previous instance of the same kind. When a payload carries no session id the reducer keys the event on a single shared `{kind}:anonymous` bucket, so every unidentified event of a kind folds into one row. This is a known limitation, not a feature: two genuinely distinct session-less instances would merge. It bites no agent today — Claude and Codex both carry a session id on their first state-bearing event (Codex's lazy `SessionStart` rides with the first `UserPromptSubmit`) — and the [instance lifecycle](#the-instance-lifecycle) is where a real per-instance key would land if a future agent emits session-less transitions.

## The state machine

An adapter emits an agent-agnostic **lifecycle signal** — the *intent* a native event carries ([`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs)) — not a final status. One pure transition function, [`step`](../../crates/rimz/src/agents/lifecycle.rs), folds that signal onto the prior state through the directed graph below; it is the single home for every transition and is reused identically for a root agent and a subagent. The reducer calls it on replay to derive the rollup, and the hook ingestion path calls it once per fresh event to log any anomaly — same table, so the two can never disagree. The five values, in ranking order — most attention-hungry first, so a working `running` agent settles *below* the calm `idle`/`success`:

- `waiting` — blocked on a human decision *(raises attention)*
- `failed` — the last turn errored *(raises attention)*
- `idle` — wired in, nothing in flight
- `success` — last turn completed cleanly
- `running` — actively working a task

The glyph, animation, and color for each are the canonical table in [the interface legend](../interface/sidebar.md#reading-the-glyphs); this doc owns the transitions, not the painting.

```text
   (none) ──registered──► idle ──turn started / subagent started──► running
                          ▲                                          │
           turn started   │   turn ended clean ──► success           │
           re-enters ─────┤   turn ended errored ──► failed   ◄───────┤
           running        │   subagent stopped ──► idle (child)       │
                          │   tool used (mutating) ──► running        │
                          └──────────────────────────────────────────┘
   compacting : prior status held, compacting head stamped (cleared by next signal)
   blocking ask pending while running ──► waiting (feed channel, not lifecycle)
   session ended / pid dead / pane reverted to shell ──► removed (no row)
```

A `TurnEnded` signal resolves the turn to `success`, or `failed` on its error bit — never back to `idle`. One exception keeps it `running`: a clean end whose signal also carries `parked_on_background` is the main thread *parking on still-in-flight background work*, not a turn end, so the row stays `running` and paints a distinct secondary `⋯ bg` marker rather than a false `✓` — the activity description stays the agent's real task, never a synthetic count (the provider-specific detection lives in [hooks.md → Appendix Claude](./hooks.md#appendix--claude-code)). An error bit still wins.

`waiting` is **not** a lifecycle transition — it is a pending blocking feed item joined to the agent (the feed channel; see [hooks.md → Two hook channels](./hooks.md#two-hook-channels)). The lifecycle channel drives the other four.

**Fail-soft, never silent.** `step` is total: an unexpected `(state, signal)` pair never panics and never freezes. It takes the signal's natural edge — the agent is authoritative about its own activity — and tags the result [`TransitionKind::Reconciled`](../../crates/rimz/src/agents/lifecycle.rs) with the state it overrode and why. The reducer discards the tag (it only wants the next state); the ingestion path ([`cli/hooks.rs`](../../crates/rimz/src/cli/hooks.rs)) logs it once per fresh event under the `rimz::agent::lifecycle` tracing target (`warn!` on a reconciled edge, `debug!` on an ignored no-op, `error!` on a quarantined identity), to stderr — never the hook stdout. So a drift between the model and reality leaves a structured, traceable breadcrumb instead of a wrong-but-quiet row. The headline reconciliation: a **mutating tool observed while the slider still reads `plan`** is impossible (plan mode is read-only), so `step` moves the posture off `plan` and logs it — the structural fix for an agent stuck reading "thinking" while it edits in auto mode.

The **displayed** status refines the rollup without changing it — `snapshot.agents` keeps the agent-owned truth; the projection ([sidebar.md](./sidebar.md)) decides what the row shows:

- a `running` agent whose permission slider is `plan` renders as **thinking**;
- a `running` agent silent past the stall window escalates to the attention `!` (see [Liveness and presence](#liveness-and-presence)) — *unless* it has a live subagent, in which case it is **waiting on its children** (a quiet wave, exempt from the stall escalation) rather than wedged;
- a resting (`idle`/`success`) agent on an account whose rate-limit window is spent projects to **`rate_limited`** — a sixth, Rimz-*derived* status ([`is_rate_limited`](../../crates/rimz/src/feed.rs)), never emitted by a hook, that joins the cockpit tally and ranks just under the actionable attention states (account-spread: every resting agent of a spent kind is parked, including one that just launched into it);
- a `compacting` head pulses over any base status while the agent condenses its context window.

`rate_limited` is the rate-limit analogue of the stall projection: both read enrichment plus liveness to refine the displayed cell, and both leave `snapshot.agents` holding the true lifecycle status.

### Plan mode as a sticky posture

`plan` is one position of the permission slider, not a separate flag: thinking is `running` joined to `permission_posture == plan`. Rimz samples posture from lifecycle observations only (`posture_from_mode`); an observation that names no slider carries the prior value forward. It never infers plan mode from prompt text or transcript content.

- **Approving a plan** moves the slider off `plan`; the next observation that reports the new posture drops the thinking state.
- **Shift-tabbing out of `plan`** mid-turn may report no new posture, but the first *mutating* tool that follows proves the agent has left read-only plan mode — `step` reconciles the posture off `plan` and logs it (see [Fail-soft, never silent](#the-state-machine)). This closes the "shows thinking while editing in auto mode" lag without inferring posture from prompt or transcript text; the trigger is a hook event (`PostToolUse` for a mutating tool), not content.
- **Subagents** own separate `agent_id`s, so a child observation never mutates its parent's posture.

The agent owns status and posture; Rimz observes and renders. `yolo` is read from the agent's own bypass flag; `interactive` folds into `default`. The vocabulary is defined once in [the interface legend](../interface/sidebar.md#reading-the-glyphs).

## Enrichment is display-only

`task`, `context_pct`, `total_tokens`, and the todo counts are **enrichment**: display-only, redactable, and they never drive routing, ranking, or a decision (the no-transcript-correctness rule). A missing value means "the agent didn't report it," never zero — the sidebar projects it to a visible 0% baseline so every observed agent paints a context bar.

Context budget is the one field no agent puts in its hook JSON — usage lives in the transcript, captured from the **transcript tail** on the turn-boundary events Rimz already fires (discovery and the per-provider mapping are [transcript.md](./transcript.md)'s concern). These are bare token counts; `payload_mode` gates the *content* of high-frequency payloads, never these gauges.

### Rich context (`AgentContext`)

Some agents publish far richer per-session data out of band than their hooks carry — context-window token accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, PR info, version, effort. The *transport* differs per agent and lives in [transcript.md](./transcript.md) (Claude's statusline feed, Codex's app-server); `observe_context` normalizes any transport into the agent-agnostic [`AgentContext`](../../crates/rimz/src/agents/mod.rs). Every field is `Option` and tolerantly parsed, so a sparse or evolved payload always parses and the renderer draws whatever subset is present. The account/balance subset (plan, metered, the rate-limit windows) is account-scoped and folds into the provider dashboard — its mapping and aggregation are [account.md](./account.md).

This is high-frequency, display-only enrichment, so it does **not** ride the event log. The feed process writes a **latest-wins per-session sidecar** — one atomic file per `(kind, agent_id)` under the runtime `agent_context/` dir — and `rimz sidebar snapshot` folds each record onto its `AgentState` (`with_agent_context`). The sidecar lives wholly off the durable path (ledger first — sidebar wakeups are latency, not truth) and dies with the session: a session-end event tombstones it, and a read past the ghost-session TTL drops it even if the tombstone was missed. It is not `payload_mode`-gated; when a loader lands, gate only the content-ish fields (PR url, output style, vim mode), never the numeric gauges. The file sits under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

## Liveness and presence

Presence comes from the live pane, not a session-exit hook: an agent renders only on the pane it stamped, and one whose pane reverts to a shell or closes is gone with no exit event required. **The binding mechanics — stamped pane id, the Codex daemon exception, jump reconciliation — live in [sidebar.md → Presence model](./sidebar.md#presence-model); this section owns only what the rollup contributes.** There is no `offline` status: a dead agent is a reverted shell row or no row, never a retracted ledger fact.

**Pid is a hygiene signal, not a render gate.** Stamped-pane binding already keeps a stale agent off a stranger's pane, so the captured pid never gates rendering. Rimz records it best-effort on each lifecycle event (`RIMZ_AGENT_PID=$PPID`, falling back to a `/proc` ancestor walk, plus the Linux process-start token to defeat pid reuse); the reaper reads *pidless* as one ghost signal.

**Per-tool activity is a heartbeat, not an event.** The durable event log is turn-grained, so `last_activity` would otherwise advance only at turn boundaries. The hook touches a per-agent heartbeat ([`agent_activity`](../../crates/rimz/src/agent_activity.rs)) on every progress-proving event — each completed tool call, the turn boundaries, subagent start/stop — and the snapshot folds the freshest touch into `last_activity`. It is *not* touched on a pre-tool event (which can fire in the same call as a blocking ask) or while blocked. This signal does three things: it keeps a busy agent's row animating (the spinner tracks real work, not a stale window), it escalates a `running` agent silent past the ~10-minute stall window to the `!` attention state, and it recovers an answered `native_ui` ask — once `last_activity` passes the ask the snapshot stops folding it, so an agent that answered in its own UI returns to `running` without waiting for the next turn boundary. Like every heartbeat it is latency, not truth: a missing file just leaves the event-log timestamp.

**The rollup reaps its own ghosts.** A session that never captured a pid and never fired a session-end event would pin a stale row forever, and relaunch-in-place or shared-pid sessions stack duplicates. At snapshot time the derived rollup (never the event log) drops two classes, both safe under one-pane-one-row: a *pidless* session past a few-hours TTL, and an *older* same-kind session on the same `(worktree_path, worktree_branch)` superseded by a strictly-newer one when the older holds no live pane the newer doesn't already occupy. An agent holding its own distinct pane is always kept. The rules are **root-only** — a subagent, with no pane of its own and no pid, is never reaped on its own and children leave the rollup only when their parent does. This is workspace-local and complements the cross-workspace `rimz gc`.
