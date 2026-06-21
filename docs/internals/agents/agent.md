# The agent model

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The per-provider native mappings — which native event means what for each agent — live in the adapter docs ([adapter/claude.md](./adapter/claude.md), [adapter/codex.md](./adapter/codex.md), [adapter/pi.md](./adapter/pi.md), [adapter/opencode.md](./adapter/opencode.md)); the account, balance, spend, and pricing model is [provider.md](./provider.md).

This doc owns how a running agent is *modeled*, the adapter boundary that produces the model's input, and the live context read-path that enriches it. The seam is a three-stage pipeline: an adapter *produces* an [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) from each native event ([The adapter boundary](#the-adapter-boundary)), this doc *folds* those observations into one [`AgentState`](../../../crates/rimz/src/feed.rs) per agent, and [sidebar.md](../sidebar/sidebar.md) *projects* that state into a row. The observation is agent-agnostic by construction, so everything below is too: a new agent that emits well-formed observations gets the state machine, ranking, liveness, and jump for free.

## The model at a glance

Four nouns carry the model:

- An **agent kind** is a wired integration (`claude`, `codex`, `pi`, `opencode`), described by an [`AgentDescriptor`](../../../crates/rimz/src/agents/descriptor.rs) whose `Capabilities` (`registers_lazily`, `subagents`, `background_tasks`, …) declare how that agent behaves. Every behavior below is capability-gated through the descriptor, so a new agent slots in by declaring what it does rather than by growing special cases.
- An **agent instance** is presence: a live local pane running a known agent right now, read from the multiplexer every tick.
- A **session** is identity: the id the agent's own hooks report, keyed `(kind, agent_id)`, where every durable fact attaches.
- The **rollup entry** is the one [`AgentState`](../../../crates/rimz/src/feed.rs) per session that ledger replay derives — the durable record the sidebar enriches and renders.

Joining instances to sessions is [the instance lifecycle](#the-instance-lifecycle); everything in between is this data flow:

```text
native agent event
  │  the adapter normalizes it             (the adapter boundary)
  ▼
AgentLifecycleObservation ──► one agent.lifecycle event in the ledger
  │  replay: reduce_agent_states folds each signal through step()
  ▼
AgentState ──► one rollup entry per (kind, agent_id)
  │  snapshot: live panes bind instances; heartbeat, sidecars,
  │  and pending asks refine the displayed row
  ▼
sidebar row                  (sidebar.md projects, the interface legend paints)
```

An agent's reduced state is two axes plus one head ([`LifecycleState`](../../../crates/rimz/src/agents/lifecycle.rs)): a **status**, the running turn's **phase** ([`TurnPhase`](../../../crates/rimz/src/agents/lifecycle.rs) — `reasoning`, `acting`, or `parked`; `idle` outside a running turn), and a transient **compacting** head painted over either. The statuses, in ranking order — most attention-hungry first, so a working `running` agent settles *below* the calm `idle`/`success`:

| Status | Meaning | Decided by |
| --- | --- | --- |
| `waiting` | blocked on a human decision | a pending blocking ask on the feed channel |
| `failed` | the last turn errored | the lifecycle channel |
| `paused` | stopped mid-turn on a provider limit | derived at projection ([Displayed status](#displayed-status)) |
| `idle` | wired in, nothing in flight | the lifecycle channel |
| `success` | last turn completed cleanly | the lifecycle channel, or projection from a rollout completion marker when no `Stop` fired ([Displayed status](#displayed-status)) |
| `running` | actively working a task | the lifecycle channel |

The glyph, animation, and color for each are the canonical table in [the interface legend](../../interface/sidebar.md#reading-the-glyphs); this doc owns the transitions, not the painting.

## The rollup

[`reduce_agent_states`](../../../crates/rimz/src/ledger/snapshot/project.rs) folds the `agent.lifecycle` events into one `AgentState` keyed by `(kind, agent_id)` — `agent_id` is the agent's session id, so two concurrent agents of the same kind never share a row. Each event is a *partial* update; how the reducer treats a field the event omits is the field's **lifetime**:

| Lifetime | Rule | Fields |
| --- | --- | --- |
| **identity** | set once when the session registers, stable thereafter | `agent_id`, `kind`, `parent_agent_id`, `agent_pid` |
| **activity** | replaced by the latest event, and *clearing* it is meaningful — an idle agent has no `task` | `status`, `task`, `last_activity` |
| **carry-forward** | persists until a newer value arrives; a missing value never resets it | `model`, `effort`, `context_pct`, `context_window`, `prompt`, `transcript_path`, `recent_prompts` |
| **live-derived** | never stored in the ledger; computed at snapshot time from the live pane or git | `pane`, `worktree_path`, `worktree_branch` |
| **transient heads** | opened and closed by signals, painted over the base status | the turn [phase](#turn-phase), the [compaction bracket](#the-compaction-bracket) (`compacting_since`; each close increments the durable `compaction_count`) |

[`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) and [`AgentState`](../../../crates/rimz/src/feed.rs) are the field catalog; the lifetimes above are the rule those types do not state. Three of the rules earn a note:

- A subagent's `task` is the one exception to the activity lifetime: it is the child's type (`Explore`, …) and carries forward as identity, so a finished child stays labeled when its `SubagentStop` omits the type.
- The live-derived fields follow the pane: it knows its current cwd every tick, so `worktree_path` and `worktree_branch` track a `git checkout`. Pinning them at registration is the branch-tracking bug (see [Liveness and presence](#liveness-and-presence)).
- `model` is stored **canonicalized** — a trailing capability tag is stripped (`claude-opus-4-8[1m]` → `claude-opus-4-8`). The tag rides only the fresh-launch payload; later events carry the bare id, so without canonicalization the carry-forward would flip `…[1m]` → `…` the first time a suffix-less event arrived. Canonicalizing at reduce time pins one stable label while the event log stays faithful to the raw payload.

### Instance identity and age

`last_activity` is always the agent's *own* latest event, never inherited from a previous instance of the same kind. Identity is required: a payload that carries no session id is quarantined — logged under `rimz::agent::lifecycle` and folded to nothing — mirroring the malformed-subagent-identity rule, so two distinct session-less instances can never merge into one row. It bites no agent today: every adapter carries a session id on its first state-bearing event (Codex's lazy `SessionStart` rides with the first `UserPromptSubmit`; OpenCode's lazy `session_created` rides with the first chat session; Pi's exists from launch), and the [instance lifecycle](#the-instance-lifecycle) is where a real per-instance key would land if a future agent emits session-less transitions.

## The state machine

An adapter emits an agent-agnostic **lifecycle signal** — the *intent* a native event carries ([`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs)). Every `agent.lifecycle` event carries its signal explicitly in the params (the writer stamps it in [`EventEnvelope::agent_lifecycle`](../../../crates/rimz/src/schema/event.rs)); a payload without one is non-conforming and folds to nothing. Which native event maps to which signal is each provider's adapter doc ([adapter/claude.md](./adapter/claude.md), [adapter/codex.md](./adapter/codex.md), [adapter/pi.md](./adapter/pi.md), [adapter/opencode.md](./adapter/opencode.md)).

One pure transition function, [`step`](../../../crates/rimz/src/agents/lifecycle.rs), folds a signal onto the prior state; it is the single home for every transition and is reused identically for a root agent and a subagent. The reducer calls it on replay to derive the rollup, and the hook ingestion path calls it once per fresh event to log any anomaly — same table, so the two can never disagree. The graph:

```text
 ●
 │ registered
 ▼
idle
 │ turn started (a mutating tool on an idle row also reconciles it)
 ▼
running ───── turn ended ─────┬── clean ─────► success ──┐
 ▲     reasoning ──► acting   │                          │
 │                            └── errored ───► failed ───┤
 │                                                       │
 └── turn started re-enters · a mutating tool on ────────┘
     success reconciles (failed holds until a new turn)

 parked     : a clean end with background work in flight stays running, phase ⋯ bg; a prompt wake resumes the same turn boundary
 subagents  : subagent started establishes the child row in running;
              subagent stopped resolves it to success / failed
 compacting : a transient head held over any status (the bracket below)
 waiting    : a pending blocking ask on the feed channel, joined at projection
 removed    : session ended · pane reverted to a shell · reaped — no row
```

The edges, precisely:

| Signal | From → to | Note |
| --- | --- | --- |
| `registered` | *(none)* → `idle` | establishes the row; with `subagent_started`, the only signal that does |
| `turn_started` | any → `running` | opens the turn in the `reasoning` phase and stamps a fresh prompt boundary; a parked running row resumes and carries the prior boundary |
| `turn_ended`, clean | `running` → `success` | the turn resolved; the phase rests |
| `turn_ended`, errored | `running` → `failed` | the error bit always wins |
| `turn_ended`, clean with background work in flight | `running` → `running` | the main thread parked, the phase is `parked`; see below |
| `subagent_started` | *(none)* → `running` | establishes the child row, keyed by the child's own id |
| `subagent_stopped` | `running` → `success` / `failed` | the child's terminal verdict, kept through the parent's turn |
| `tool_used` (mutating) | resting or *(none)* → `running`, reconciled | completed work proves a turn; attention rows hold; the first file-editing tool moves the phase to `acting` |
| `compacting` | status and phase held | stamps the [compaction head](#the-compaction-bracket) |
| `compaction_ended` | auto → `running` (phase carried) · manual → `idle` · trigger unknown → held | closes and counts an open [bracket](#the-compaction-bracket) |
| `ended` | removal | the reducer's tombstone path handles it upstream; reaching `step` it is an ignored no-op |

A `TurnEnded` signal resolves the turn to `success`, or `failed` on its error bit — never back to `idle`. One exception keeps it `running`: a clean end whose signal also carries `parked_on_background` is the main thread *parking on still-in-flight background work*, not a turn end, so the row stays `running` in the `parked` phase and paints a distinct secondary `⋯ bg` marker rather than a false `✓` — the activity description stays the agent's real task, never a synthetic count (the provider-specific detection lives in [adapter/claude.md](./adapter/claude.md#hooks-and-lifecycle)). Claude wakes a parked parent by injecting the finished background task's notification as a `UserPromptSubmit`; folded on a parked running row, that `TurnStarted` resumes the same logical turn and carries `turn_started_at` forward, so child verdicts stay visible through the delegation wave. A real prompt submitted while the row is still parked follows the same signal-level edge and carries the prior boundary; once the turn reaches a clean end, the next prompt stamps fresh and clears past-turn verdicts. An error bit still wins. A `SubagentStopped` signal resolves the *child* entity the same way — `success`, or `failed` on its error bit (Claude maps a non-zero `exit_code`; Codex reports no subagent error signal, so its children always resolve clean) — and the sidebar keeps that `✓`/`!` result through the parent's turn ([sidebar.md → Sub-agent lists](../sidebar/sidebar.md#sub-agent-lists)).

`waiting` arrives on the feed channel: a pending blocking ask joined to the agent puts the row in `waiting` ([Two hook channels](#two-hook-channels)); the lifecycle channel drives the other four lifecycle statuses.

**Fail-soft, never silent.** `step` is total: an unexpected `(state, signal)` pair never panics and never freezes. It takes the signal's natural edge — the agent is authoritative about its own activity — and tags the result [`TransitionKind::Reconciled`](../../../crates/rimz/src/agents/lifecycle.rs) with the state it overrode and why. The reducer discards the tag (it wants the next state and the transition facts); the ingestion path ([`cli/hooks.rs`](../../../crates/rimz/src/cli/hooks.rs)) logs it once per fresh event under the `rimz::agent::lifecycle` tracing target (`warn!` on a reconciled edge, `debug!` on an ignored no-op, `error!` on a quarantined identity), to stderr — hook stdout stays the decision channel. So a drift between the model and reality leaves a structured, traceable breadcrumb instead of a wrong-but-quiet row. The headline reconciliation is in the edge table: a **tool observed on a resting row** proves the rollup is stale — the agent is working — so `step` moves it to `running` and logs the edge.

**Extending the signal vocabulary.** A new [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs) variant requires both: (a) a concrete native event on a shipping provider that no existing variant plus enrichment expresses, and (b) a distinct `(status, phase)` edge in `step` — landed with its edge test and the totality test extended. `CompactionEnded` is the worked example: three providers close the bracket with different evidence and one optional trigger bit, and the same signal owns all three edges ([the bracket](#the-compaction-bracket)). Anything short of both is enrichment on an existing signal: Pi's `stopReason: "aborted"` rides `TurnEnded { errored: true }`, queued prompts and external waits have no observable native event, and a `Verifying` phase has no provider that emits one.

### Turn phase

The phase is the running turn's shape, derived from the turn's own hook events — the agent owns its status, Rimz derives the phase. Every turn opens in `reasoning`: `TurnStarted` and `SubagentStarted` set it, and the sidebar paints the themeable thinking head from the [interface legend](../../interface/sidebar.md#reading-the-glyphs) while the turn reads, searches, and decides. The turn's first **file-editing** tool moves it to `acting` — `ToolUsed { edits: true }`, each adapter's file-writing subset of its mutating set (Claude `Edit`/`Write`/`MultiEdit`/`NotebookEdit`, Codex `apply_patch`, Pi `edit`/`write`, OpenCode `edit`/`write`/`apply_patch`/`patch`), read through `tool_edits_files`. The trigger is always a hook event, never prompt or transcript content.

```text
turn starts ──► reasoning  ──first file-editing tool──► acting ──► turn ends
                    │                                                  ▲
                    └── a research turn that never edits a file ───────┘
clean end with background work still in flight ──► parked (the row stays running, ⋯ bg)
```

- **A research turn stays in the thinking head end to end** — searches, reads, and shell commands write no file, so a turn that answers without editing stays in `reasoning`.
- **A shell command is work without writing**: it keeps the row live and leaves the phase in place. A phase that left `reasoning` never re-arms mid-turn, and a parked turn that runs a tool is visibly back at work in `acting`.
- **Any turn boundary rests the phase** — `TurnEnded` and `SubagentStopped` drop it; the next prompt re-arms it. A clean end with background work still in flight parks it instead.
- **Subagents** own separate `agent_id`s, so a child observation never mutates its parent's phase — and the lifecycle channel is bracket-grained for children: only `SubagentStarted`/`SubagentStopped` fold to the child's rollup, a child's per-tool events are dropped at the adapter ([adapter/claude.md → In-subagent attribution](./adapter/claude.md#in-subagent-attribution)), and the sidebar's child entry carries status only.

No expiry window is needed: a turn that goes silent escalates through the stall projection regardless of its phase. The phase vocabulary is painted once, in [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

### The compaction bracket

Compaction is a transient head over the status: the opening signal (`Compacting` — Claude/Codex `PreCompact`, Pi `session_before_compact`, OpenCode `session_compacting`) stamps `compacting_since` and holds the prior status and phase, so the sidebar pulses the compaction head over whatever the agent was doing. The session's next lifecycle signal closes the bracket — `step` emits the close as a transition fact ([`Transition::compaction_closed`](../../../crates/rimz/src/agents/lifecycle.rs)), and the rollup increments the session's durable `compaction_count` from it, exactly once per bracket; the card surfaces it as `↻ N` on the context line.

`CompactionEnded` is the explicit close — Claude/Codex `PostCompact`, Claude/Codex `SessionStart { source: "compact" }` as triggerless close evidence, Pi `session_compact` with no trigger bit, OpenCode `session_compacted` — and its trigger decides where the agent lands: a known automatic trigger returns to `running` with the interrupted phase carried, because automatic compaction happens mid-turn; a known manual trigger rests to `idle`, because `/compact` runs between turns; an absent trigger holds the prior status and phase. A close that rests the agent — a manual `/compact`, or a triggerless close from a rested state — advances `turn_started_at`, retiring the prior turn's subagents the same as a fresh prompt or a `/clear`; an automatic mid-turn close resumes the turn and holds the boundary ([sub-agent lists](../sidebar/sidebar.md#sub-agent-lists)). Redundant close signals are idempotent because an absent bracket closes nothing. The projection also expires the head past a short display window, so a crash mid-compact with no later signal can never pulse it forever.

## Displayed status

`snapshot.agents` keeps the agent-owned truth; the projection ([sidebar.md](../sidebar/sidebar.md)) refines what the row *shows* on top of it, folding enrichment and liveness into the displayed cell. The refinements are one family with a pinned precedence — top rung wins:

1. **A human-blocked `waiting` row stays first** — a pending blocking ask outranks every refinement below.
2. **`paused`** — an agent whose latest turn stopped on a provider limit. Rimz derives this status at projection (no hook emits it); it joins the cockpit tally and ranks just under the actionable attention states. The marker can refine a still-`running` row when the provider emitted no lifecycle end, or a same-turn `failed` row when the lifecycle did record an errored end. The certificate is per-agent: Claude's `StopFailure` hook writes it precisely and Claude transcript-tail detection is the no-Stop backstop; Codex rollout detection on `Stop` first marks the lifecycle end `failed`, then the same marker refines that row to `paused` while the budget is still spent. `rate_limit` stays paused while any known spent window for that kind remains unreset and lifts to `failed` once every known spent window has reset if no newer hook event self-clears it; `overloaded` stays paused until a newer hook event self-clears it ([provider.md → Spent windows](./provider.md#spent-windows-and-paused-rows)).
3. **Waiting on children** — a `running` agent with a live subagent paints a quiet wave, exempt from the stall escalation. The stall clock reads the row's displayed activity, which folds in the children's ([sidebar.md → Sub-agent lists](../sidebar/sidebar.md#sub-agent-lists)), so a child that just finished defers the escalation too.
4. **Turn death** — a non-pause provider API error escalates to `!` at once. For a still-`running` row, the marker postdates `last_activity`, so the explicit death certificate beats the stall window. For a terminal `failed` row, the marker must fall inside the row's current turn (`turn_started_at` or later), so an old marker never explains a fresh failure. The card quotes the upstream error text, and any newer hook event (a prompt, a resume, a rewind) self-clears it.
5. **Turn completion** — a `running` row whose latest turn finished without a `Stop` hook settles to `success`. Codex's `/review` runs in review mode and ends on a clean rollout `task_complete` with no `Stop`, so the lifecycle stays `running`; the completion marker postdates `last_activity` and settles the row instead of letting the stall window misread a finished review as failed ([adapter/codex.md → Turn-completion marker](./adapter/codex.md#turn-completion-marker)). A turn-death marker outranks it, and a newer prompt self-clears it the same way.
6. **Stall** — a `running` agent silent past the configurable stall window projects to `paused` only when its kind has a spent, unreset window; otherwise it escalates to the attention `!` (see [Liveness and presence](#liveness-and-presence)).

Each rung reads enrichment plus liveness to refine the displayed cell, and each leaves `snapshot.agents` holding the true lifecycle status: Claude transcript death can leave the rollup `running`, while Codex Stop-over-rollout-error records the rollup as `failed` and lets projection refine the display to `paused` when appropriate. The order is a pinned contract: the [`displayed_status_precedence_ladder_holds`](../../../crates/rimz/src/ledger/snapshot/view/tests/status/stall.rs) projection test stacks the causes per rung and asserts which one wins, so a reordering fails the suite even when every single-cause test still passes; the completion rung's placement below turn death is pinned by the [`turn_complete`](../../../crates/rimz/src/ledger/snapshot/view/tests/status/turn_complete.rs) projection tests. The phase and head paints ride over this base: a `running` agent in `reasoning` renders the thinking head ([Turn phase](#turn-phase)), and an open [compaction bracket](#the-compaction-bracket) pulses over any base status.

## The instance lifecycle

An agent reaches the sidebar as an **agent instance** — a live local pane running a known agent (`command_agent_kind`), bound one-to-one to its pane id, `pane_pid`, and process-start (derived from the in-pane CLI's `/proc` entry when the backend reports none natively, as Zellij does). A **session** binds to it, and the instance exits when its pane reverts to a shell. The instance exists before any session id is known, and the lifecycle's one hard problem is joining the two — which turns on two independent axes: how the hook reports its identity, and where the agent runs.

**Hook identity — stamped or daemon-routed.** A **standalone** agent runs in its pane, so the hook is a descendant of it and reads the pane's `ZELLIJ_PANE_ID` / `TMUX_PANE` and pid directly: it **stamps** the pane id onto the session. A **daemon-routed** agent runs through a background daemon, so the hook fires from the *daemon* — no pane env, the *daemon's* pid (shared by every client) — and the session is **unstamped** (no pane id). Claude is standalone. Codex 0.137+ daemon-routes plain TUI hooks and `codex remote-control start` hooks through the shared app-server, so in-pane Codex sessions are unstamped and bind through the recovery ladder below ([adapter/codex.md](./adapter/codex.md#session-registration-and-launch-quirks)). Pi and interactive OpenCode run their integrations in-process in the pane, so they are standalone ([adapter/pi.md](./adapter/pi.md), [adapter/opencode.md](./adapter/opencode.md)).

**Presence — in-pane or remote.** Orthogonal to hook identity is where the agent actually runs:

- **In-pane.** A local pane runs the agent, with its own client `pane_pid`, so the user can jump to it. A standalone agent binds its pane by the stamped id; a daemon-routed *in-pane* agent (a Codex CLI thin-wrapping the daemon) is unstamped and binds through the recovery ladder below. Either way it renders as a normal, jump-able agent row.
- **Remote.** The agent runs only in the daemon, with no local pane — `claude remote-control --spawn worktree`, or a Codex thread started from the web. It carries a worktree but no `pane_pid` and nothing to focus. **Rimz does not render remote agents yet — a documented gap, deferred to a future round ([sidebar.md → Presence model](../sidebar/sidebar.md#presence-model)).** The `claude remote-control` host pane itself is separate infrastructure, filtered out of the room ([`pane_is_host`](../../../crates/rimz/src/remote_control.rs)) and surfaced as the `⇅ rc` flag.

So the binding test is one question: does a live local pane bind the session? A stamped session binds by id; an unstamped session binds through the recovery ladder; and a session no pane binds is a remote agent.

**Phase 1 — pre-session presence (instance idle).** A wired agent instance with no bound session yet renders as an idle agent row, so a just-launched agent reads as itself rather than a bare process. Claude reaches this through a real `SessionStart` at launch, so its instance and session coincide immediately. An agent that registers its session lazily — Codex fires no `SessionStart` until the first prompt ([adapter/codex.md → Session registration](./adapter/codex.md#session-registration-and-launch-quirks)), and OpenCode creates a session once its TUI server has a real chat session ([adapter/opencode.md → Hooks and lifecycle](./adapter/opencode.md#hooks-and-lifecycle)) — has an instance on screen before any session, so Rimz synthesizes an idle `○ <kind>` row for it until the first turn binds the real session ([`idle_agent_row`](../../../crates/rimz/src/ledger/snapshot/panes/lazy.rs), gated on the kind being wired). The capability is the agent's: the descriptor's `registers_lazily` flag opts an agent into the synthesis and the cwd-bind, so a new lazy agent slots in without bespoke sidebar code, while Claude (always stamped) declares the opposite and stays unaffected. The general rule: a wired instance with no session is an idle agent; an unwired one stays a [process row](../sidebar/sidebar.md#process-rows).

**Phase 2 — session binding.** A lifecycle hook arrives carrying a session id, and Rimz joins it to the right instance. A standalone hook stamped the pane id, so the join is exact and free. An unstamped session walks a deterministic recovery ladder: hook ingestion first writes a recovered same-cwd pane stamp from the repaired live frame; a `codex resume <session-id>` pane binds exactly; then same-cwd sessions pair newest-first to the latest viable pane process start before the session's first event. Residual ambiguity binds deterministically and appends a runtime `binding.log.jsonl` breadcrumb. The ladder's guards, disambiguation rules, and limits are [sidebar.md → Presence model](../sidebar/sidebar.md#presence-model).

**Phase 3 — instance exit.** The in-pane agent process is the liveness truth, surfaced through the pane: the CLI client is the pane's foreground process, so when it exits the pane reverts to a shell and stops reading as an agent — the instance leaves with no exit hook, in both launch modes. A `SessionEnd` hook (Claude) tombstones the session eagerly on top of this, so its pending asks and context sidecar clear at once; Codex has no `SessionEnd`, so a Codex session leaves by pane liveness and the [rollup reaper](#liveness-and-presence) alone. One residue is daemon-routed-only: an unstamped session whose in-pane CLI has exited still sits in the rollup, and its recorded `agent_pid` is the shared daemon, which outlives the client — so the reaper's pid hygiene cannot clear that unstamped remnant once the client's pane is gone. The app-server's loaded-thread set is the signal that does: the producer reaps a daemon-mode session absent from `thread/loaded/list` before the pane fold ([`drop_dead_daemon_sessions`](../../../crates/rimz/src/ledger/snapshot/view/reap.rs)), so its stale stats never reach a live pane, while an unreachable daemon or an untrusted list keeps every session ([sidebar.md → Presence model](../sidebar/sidebar.md#presence-model)).

## Liveness and presence

Presence comes from the live pane, with no exit event required: an agent renders only on the pane it stamped, and one whose pane reverts to a shell or closes is gone on the next snapshot. **The binding mechanics — stamped pane id, the Codex daemon exception, jump reconciliation — live in [sidebar.md → Presence model](../sidebar/sidebar.md#presence-model); this section owns only what the rollup contributes.** There is no `offline` status: a dead agent is a reverted shell row or no row, never a retracted ledger fact.

**Stamped-pane binding decides what renders; the captured pid feeds the reaper.** Rimz records the pid best-effort on each lifecycle event (`RIMZ_AGENT_PID=$PPID`, falling back to a `/proc` ancestor walk, plus the Linux process-start token to defeat pid reuse), and the reaper reads *pidless* as one ghost signal — stamped-pane binding already keeps a stale agent off a stranger's pane, so the pid never gates rendering.

**Per-tool activity rides a runtime heartbeat.** The durable event log is turn-grained, so `last_activity` would otherwise advance only at turn boundaries. The hook touches a per-agent heartbeat ([`agent_activity`](../../../crates/rimz/src/agent_activity.rs)) on every progress-proving event — each completed tool call, the turn boundaries, subagent start/stop — and the snapshot folds the freshest touch into `last_activity`. It is *not* touched on a pre-tool event (which can fire in the same call as a blocking ask) or while blocked, and it is keyed by the event's own session (`agent_id` first, then `session_id`): a backgrounded subagent's progress touches the *child's* heartbeat, never the parent's, so a parent blocked on an ask keeps its `last_activity` frozen until it acts. The display-only fold from child activity onto the parent row lives in [sidebar.md → Sub-agent lists](../sidebar/sidebar.md#sub-agent-lists). This signal does three things: it keeps a busy agent's row animating (the spinner tracks real work, not a stale window), it escalates a `running` agent silent past the configurable stall window (30 minutes by default) to the `!` attention state, and it recovers an answered `native_ui` ask — once `last_activity` passes the ask the snapshot stops folding it, so an agent that answered in its own UI returns to `running` without waiting for the next turn boundary. Like every heartbeat it is latency, not truth: a missing file just leaves the event-log timestamp.

**The rollup reaps its own ghosts.** A session that never captured a pid and never fired a session-end event would pin a stale row forever, and relaunch-in-place or shared-pid sessions stack duplicates. At snapshot time the derived rollup (never the event log) drops two classes, both safe under one-pane-one-row: a *pidless* session past a few-hours TTL, and an *older* same-kind session a strictly-newer one supersedes by taking over its exact pane — the pane is the unit of identity, so a branch checkout between the two does not split them — or, for two paneless remnants, on the same `(worktree_path, worktree_branch)`. An agent holding its own distinct pane is always kept. The rules are **root-only** — a subagent, with no pane of its own and no pid, is never reaped on its own and children leave the rollup only when their parent does. The expanded-list projection lives under the parent row ([sidebar.md → Sub-agent lists](../sidebar/sidebar.md#sub-agent-lists)). This is workspace-local and complements the cross-workspace `rimz gc`.

## The adapter boundary

A coding agent reports to Rimz through hooks. Every agent — Claude, Codex, Pi, OpenCode, and every future one — speaks through one trait, [`AgentAdapter`](../../../crates/rimz/src/agents/mod.rs), registered in [`registry::ADAPTERS`](../../../crates/rimz/src/agents/registry.rs). The trait is the single place a native protocol diverges and the single place it is normalized; nothing downstream of it is agent-specific. The per-provider mappings it produces are the adapter docs ([adapter/claude.md](./adapter/claude.md), [adapter/codex.md](./adapter/codex.md), [adapter/pi.md](./adapter/pi.md), [adapter/opencode.md](./adapter/opencode.md)); the raw upstream protocols they read are the external references ([claude-reference.md](../../externals/agent-adapter/claude-reference.md), [codex-reference.md](../../externals/agent-adapter/codex-reference.md), [pi-reference.md](../../externals/agent-adapter/pi-reference.md), [opencode-reference.md](../../externals/agent-adapter/opencode-reference.md)).

Agents are *sources*, not a privileged path. Anything a hook does, a script can do through the same CLI — a hook is just an adapter that translates a native protocol onto `rimz event`/`rimz feed`.

### The seam — `AgentAdapter`

Adding an agent is implementing the trait plus a static [`AgentDescriptor`](../../../crates/rimz/src/agents/descriptor.rs) (identity, branding, capabilities, tool tables, and integration coverage) and one registry line. Its methods, by role (signatures live in the trait):

- **`classify_hook`** sorts a native event into one of the two channels below (or `Unknown`, dropped) and, for a blocking event, names the [`FeedKind`](../../../crates/rimz/src/feed.rs).
- **`observe_lifecycle`** is the normalizer: it maps a native lifecycle event onto one [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) — the unified event shape every downstream reducer reads. `None` means "no state transition here", so high-frequency events stay silent.
- **`render_decision`** / **`render_neutral`** emit the agent-native decision JSON when a resolver answers, and the neutral no-op when no one does.
- **`hook_cap`** (a descriptor field) is how long a blocking hook may park on the bridge before falling back to neutral — set from the upstream's published deadline.
- **`observe_context`** normalizes a rich out-of-band payload into [`AgentContext`](../../../crates/rimz/src/agents/context.rs); **`local_context_refresh`** derives sidecar fields from local provider state during non-blocking progress hooks; `ends_session` / `moves_on` mark the events that expire a session's pending asks.
- **`remote_control_status`** reads provider-owned local settings that affect remote-control display state; the static ability to drive pane or background sessions stays in the descriptor's `remote_control` capability.
- **`probe_version`** defaults to the shared descriptor-resolved `--version` probe — `$PATH` first, then the descriptor's known install dirs; adapters override only when the binary name or a richer primary transport differs.
- **`install_hooks`** / **`preview_hook_install`** / **`uninstall_hooks`** / **`hooks_installed`** own the per-user config write and report it (gated by the descriptor's `hook_install` capability).
- **`probe_account`** / **`parse_spend`** / **`transcript_files`** feed the account and spend model in [provider.md](./provider.md).

Two invariants hold the seam shut:

- **Adapters never touch the ledger.** The adapter is a pure mapper. The [`rimz hooks feed`](../../../crates/rimz/src/cli/hooks.rs) CLI owns every ledger write and all bridge I/O; it calls the adapter for classification and rendering only.
- **Nothing downstream reads a native payload.** The adapter emits exactly two things the rest of Rimz consumes — an `AgentLifecycleObservation` and a decision `Value`. A native field reached for outside an adapter is a mapping that belongs *in* the adapter.

### Two hook channels

`classify_hook` sorts every native event into one of two wired channels. The distinction is whether the hook can hold the agent open while Rimz waits for an answer.

**Lifecycle — fast, non-blocking.** Drives agent status, the turn phase, task, enrichment, and `rimz feed list --audit` depth. Each flows through `observe_lifecycle`; an event that carries no transition returns `None` and records nothing.

**Blocking-feed — holds the agent open.** A permission request, plan approval, or user question. It becomes a [`FeedItem`](../../../crates/rimz/src/feed.rs), not an observation, and engages the three operating paths in [ledger.md](../sidebar/ledger.md#the-three-paths-at-a-glance): bind a per-request socket and wait for a resolver (`bridge`), or write the item and return neutral so the agent's own UI asks (`native_ui`). The `native_ui` hand-off requires a surface to hand to: an agent whose descriptor declares `native_ask_ui` off (pi) resolves the same ask neutrally with **no feed item** — there is nothing the item could route the human to, so pushing one would strand it waiting.

Blocking decision hooks must be **sync**. Installing one as async is a hard error — the agent would ignore the decision printed on stdout — and the installer rejects it.

### Hook stdout is the decision channel

This is the canonical statement of the rule the rest of the docs link to. A hook's stdout carries exactly one thing: the agent-native decision JSON, printed only when a resolver answers on the bridge. The neutral path prints nothing and exits 0 — the agent's own UI takes over. It follows that:

- **Logs never go to stdout.** They go to stderr or Rimz runtime state logs such as `binding.log.jsonl` (the `print_stdout` lint gates this — see [rust-conventions.md](../../contributing/rust-conventions.md)).
- **Hook helper children get fresh, fully-piped stdio — never inherited.** A wrapped statusline command's stderr, a notification helper's chatter, must never leak onto the decision channel. A CI grep rejects `Stdio::inherit` in hook paths.
- **Every neutral and decision shape is golden-tested**, including the neutral no-op (see [Adding an agent](#adding-an-agent)).

### Hooks resolve the room they live in

A hook resolves its workspace as a **participant** ([`WorkspaceResolver::resolve_participant`](../../../crates/rimz/src/workspace.rs)): the session's identity pin — `RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT`, stamped into the mux environment at birth ([multiplexers.md → The identity pin](../sidebar/multiplexers.md#the-identity-pin)) — wins over re-deriving identity from cwd, so an agent working inside a nested repo in a directory room writes to the room its pane lives in, never to a ledger no sidebar reads. The pin is hash-verified (`workspace_id` must hash from the pinned root) and any mismatch falls through to the static ladder (git → marker → directory): a hook on the agent's critical path degrades on identity, never errors. Every participant surface resolves the same way — `rimz event`/`feed`, the statusline helpers, the pane verbs, the sidebar renderer — and a CI grep (`cargo xtask invariants`) keeps the create-mode resolver out of them; room-choosing commands (`rimz start`/`attach`, maintenance) resolve statically, so a deliberate per-repo room can still be created from inside a parent room.

A **daemon-routed** hook (Codex's, fired from the shared per-user app-server — see [adapter/codex.md](./adapter/codex.md#session-registration-and-launch-quirks)) inherits its daemon's environment, not the pane's, so the env pin never reaches it. `rimz hooks feed` recovers the pin from the sibling agent process instead ([`WorkspaceResolver::resolve_participant_with_pin_recovery`](../../../crates/rimz/src/workspace.rs)): the daemon spawns hooks with the session cwd, so the in-pane agent process sharing that cwd carries the pane's pin in `/proc/<pid>/environ`. Each candidate is verified like the env pin and adopted only when every candidate names one root; an empty or split scan — and any non-Linux host — degrades to the static ladder. The full order: `--root`, env pin, recovered sibling pin, static ladder.

Session-to-pane binding diagnostics use the `rimz::agent::binding` tracing target and the workspace runtime `binding.log.jsonl`: exhausted daemon focus recovery and non-start events creating unseen sessions warn to the log stream, while each recovery attempt appends its probes, candidates, reject reasons, and outcome to the JSONL file. Hook stdout stays reserved for the decision channel.

### From native event to internals

A lifecycle hook fires → `classify_hook` returns `Lifecycle` → `observe_lifecycle` maps the payload onto an `AgentLifecycleObservation` → the CLI records it as an `agent.lifecycle` event. The observation is the contract boundary; from there [the rollup](#the-rollup) and [the state machine](#the-state-machine) above own it. A new agent that emits well-formed observations gets all of that for free.

A blocking hook fires → `classify_hook` returns `BlockingFeed` with a `FeedKind` → the CLI writes a feed item and runs the three operating paths. On a resolver answer it calls `render_decision` and prints the JSON; on timeout (the `hook_cap`) or with no fresh resolver it calls `render_neutral` and the agent's UI takes over (an agent with no native ask UI just proceeds — neutral is its allow). The per-request socket, CAS, nonce, and late-answer rules live in [ledger.md](../sidebar/ledger.md).

### Hook install — the visible security step

Installing hooks edits the agent's own config, so it is a security surface, never silent. `rimz start` detects installed, supported agents each run, prints a per-agent consent prompt, one agent at a time, with the config path, additive impact, undo command, and a `d` diff; installs the approved agents after the prompts finish; and continues if the user skips. `rimz hooks install` installs every detected supported agent on PATH and returns a JSON array; `rimz hooks install <agent>` keeps the single-agent JSON object. `rimz hooks uninstall` removes every installed Rimz-managed hook set even when the agent binary is no longer on PATH and prints `[]` when nothing is installed; `rimz hooks uninstall <agent>` keeps the single-agent object. `hooks_installed()` makes the state observable: `rimz doctor` reports it per agent. An agent run before its hooks are installed fires nothing and is invisible — never silently broken.

**What install wires.** Every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment and audit depth current. Codex uses those progress events to stat-gate the rollout tail and push local token/cost context without waiting for the app-server. The single source of truth for the wired set is each adapter's `INSTALLED_EVENTS`-style constant — not restated here. Install detection requires the full canonical set, so an under-wired config re-offers the idempotent merge. Per-tool payload *content* is gated by `[privacy] payload_mode` (`metadata` / `redacted` / `full`; see [configuration.md](../../reference/configuration.md#sidecars-and-privacy)); the gate strips content, never whether a transition is observed.

**The installed config shape.** Claude and Codex have no wildcard event key, so install writes one block per wired event into their config files; Pi and OpenCode instead own one whole integration file (see [adapter/pi.md](./adapter/pi.md), [adapter/opencode.md](./adapter/opencode.md)). Inside the config-merge shape it stays minimal:

- **One command for every event** — `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`, with no `--event`. The helper reads the event from the payload's `hook_event_name`; `--event` survives only as a manual debugging override.
- **Idempotent, self-healing reclaim.** Install reclaims every rimz-owned entry by the stable command substring `rimz hooks feed --source <agent>` — marked or not, with or without a legacy `--event` — then rewrites the canonical set, so duplicate or stale blocks never accumulate. User-authored hooks (no rimz command) are untouched.

The per-agent install caps, config formats, and trust-state quirks live in each adapter doc. **Trust.** Every hook command enters the executable-surface hash, so a tampered hook config demotes project trust to stale (see [trust.md](../sidebar/trust.md)).

## Enrichment

The ledger and explicit events decide routing, ranking, and state; enrichment paints the row. `task`, `context_pct`, `context_window`, `total_tokens`, and the todo counts are **enrichment**: display-only and redactable (the no-transcript-correctness rule). A missing value means "the agent didn't report it," never zero — the sidebar projects it to a visible 0% baseline so every observed agent paints a context bar.

`context_window` is the model's window in tokens, and uniformly across agents it is the model's max **input** tokens: the gauge numerator counts input-side occupancy only (`input + cache`, never output — see [`context_used_tokens`](../../../crates/rimz/src/feed.rs)), so a model that splits its window into separate input and output caps scales against the input cap. Claude resolves it from the payload model id, where the `[1m]` marker widens it; Codex reads the rollout's `model_context_window` and uses its 272k provider fallback until the rollout names the exact window; OpenCode resolves it from its model catalog (`Model.limit.input`, falling back to the total `Model.limit.context` for a model with no separate input cap). The card's identity line renders it (`258k`, `1M`), preferring the fresher out-of-band reading from [`AgentContext`](#rich-context-agentcontext) when one exists.

Context budget is the one field no agent puts in its hook JSON — usage lives in the transcript or in a provider-owned in-process gauge (the [two origins](#two-sources) below; the per-provider mapping is each adapter doc's concern). Claude captures transcript usage on turn-boundary lifecycle events because statusline owns its live sidecar. Codex captures the rollout tail on progress hooks and through a producer backstop, stat-gated so an unchanged file is one stat and a changed file is a bounded tail read. These are bare token counts; `payload_mode` gates the *content* of high-frequency payloads, never these gauges.

### Two sources

A session's context data has two origins. Both flow through one adapter — the [`AgentAdapter`](../../../crates/rimz/src/agents/mod.rs) — and normalize onto the same internal fields, so a new provider implements one or both and the rest of Rimz is unchanged. Enrichment is **never correctness**: a missing file, a torn line, an absent agent each degrades to an omitted field, never a failed hook or a wrong decision.

**The transcript/store read path** is the universal floor. Every provider has a local usage store that the spend parser already understands — JSONL for Claude, Codex, and Pi; SQLite for OpenCode — so the agent row can derive a session dollar total from the same `parse_spend` path that feeds history. For Claude this stays a low-frequency fallback because statusline owns the live `AgentContext`. For Codex the rollout tail is also the native live token/cost source: progress hooks and the elected snapshot producer run a stat-gated local refresh that reads a bounded tail only when `(mtime, nanos, len)` changes, merges tokens/cost into the runtime sidecar, and wakes the sidebar. For Pi and OpenCode, a turn-ended lifecycle signal resolves the current session store, sums that session's spend entries, merges the cost into the sidecar, and declares `live$` as partial coverage because the figure is reconstructed on turn end rather than provider-pushed realtime data.

**The rich-context transport** is the provider-specific upgrade, where a provider offers one. It carries everything the local read cannot or should not derive — rate-limit windows, account plan, PR info, model display name, version — on that provider's own cadence. Each provider's transport differs and is documented in its adapter doc (Claude pushes statusline JSON, Codex reads read-only app-server methods); transport payloads normalize through `observe_context` into one [`AgentContext`](../../../crates/rimz/src/agents/context.rs), while local transcript refreshes use the adapter's separate `local_context_refresh` hook.

A provider whose hook wire Rimz authors has a third option: stamp the gauge **onto the hook payload itself**. Pi's extension does this on every envelope from its in-process context API, adds the latest per-call split, and carries best-effort response-header rate-limit windows as sparse sidecar context, so its live context needs neither a transcript tail nor a general transport ([adapter/pi.md](./adapter/pi.md#context-and-transcript)). OpenCode's plugin uses the same authored-wire path, maintaining its gauge from `message.updated` events and stamping the latest split — plus the model's context window, resolved from OpenCode's own model catalog read in-process — onto each lifecycle envelope ([adapter/opencode.md](./adapter/opencode.md#context-and-transcript)).

### Reading rules

The tail reader is provider-agnostic ([`read_transcript_tail`](../../../crates/rimz/src/agents/mod.rs)) and every adapter parses on top of it under the same rules:

- **Bounded.** Read at most the trailing 64 KB, so a multi-megabyte log never stalls a hook.
- **Newest-first.** Scan lines in reverse and take the most recent usage record; a truncated leading line from the tail seek simply fails to parse and is skipped.
- **Stop when found.** Bail as soon as the needed records are in hand (Codex tracks the latest `token_count` and `model` separately and stops once both are filled).
- **Lossy and forgiving.** Decode as lossy UTF-8; any IO or parse failure yields empty fields.
- **Zero vs unknown.** A transcript that opens cleanly but carries no usage yet is a *fresh* session — report an explicit `0%` so the bar draws empty rather than vanishing. A transcript that cannot be read stays unknown (`None`): "the agent did not report it," never a false zero.

### Rich context (`AgentContext`)

Some agents publish far richer per-session data out of band than their hooks carry — context-window token accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, thread preview/name, PR info, version, effort. The *transport* differs per agent and lives in its adapter doc: Claude pushes statusline JSON, Codex merges rollout-derived local usage and detached app-server metadata, Pi forwards response-header windows on the authored hook envelope, and Pi/OpenCode get a turn-end session cost supplement from their local spend stores. `observe_context` normalizes transport payloads into the agent-agnostic [`AgentContext`](../../../crates/rimz/src/agents/context.rs), while adapter-specific local refreshes and the shared spend supplement produce the sidecar fields that do not come from a transport. Every field is `Option` and tolerantly parsed, so a sparse or evolved payload always parses and the renderer draws whatever subset is present. The account/balance subset (plan, metered, the rate-limit windows) is account-scoped and folds into the provider dashboard — its mapping and aggregation are [provider.md](./provider.md).

This is high-frequency, display-only enrichment, so it does **not** ride the event log. Rimz writes a **latest-wins per-session sidecar** — one atomic file per `(kind, agent_id)` under the runtime `agent_context/` dir — from CLI producer paths: statusline feed, hook ingestion/local transcript refresh, detached refresh helpers, and the elected snapshot producer's Codex stat-gated backstop. `rimz sidebar snapshot` folds each record onto its `AgentState` (`with_agent_context`). The sidecar lives wholly off the durable path (ledger first — sidebar wakeups are latency, not truth) and dies with the session: a session-end event tombstones it, and a read past the ghost-session TTL drops it even if the tombstone was missed. It is not `payload_mode`-gated; when a loader lands, gate only the content-ish fields (PR url, output style, vim mode), never the numeric gauges. The file sits under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

## Adding an agent

Claude, Codex, Pi, and OpenCode are the worked adapter examples. New agents such as Cursor, Gemini, Copilot, and similar tools land through `AgentAdapter` once their hook surface and decision outputs are verified. The work is one new directory under [`crates/rimz/src/agents/`](../../../crates/rimz/src/agents/AGENTS.md) — the trait impl, its `AgentDescriptor`, typed payloads, and `spend.rs` — plus one line in `registry::ADAPTERS` and a new doc under [`adapter/`](./adapter/claude.md) (the native-event → internal mapping). Nothing else changes: spending, doctor, install, branding, and classification all resolve through the registry. The descriptor's `coverage` table is the integration checklist: every `IntegrationConcern` is declared `Wired { via }`, `Partial { via, gap }` — no native signal, but the behaviour reconstructed by derivation, with the gap named — or `Unsupported { reason }`, conformance checks it against capabilities, installed events, the classification corpus, and the realtime-cost spend fixture. The descriptor's `lifecycle_hooks` table declares the lifecycle-signal grid: every `LifecycleSignalKind` is `Native { event }`, `Derived { via, gap }`, or `Absent { reason }`, and conformance checks native event names against installed hooks and the corpus. `rimz doctor` prints both matrices — product coverage and hooks coverage — wired green, partial yellow, unsupported/absent dim, so absences are visible at a glance. Codex and OpenCode both use partial `end` and `idle`: no per-session end hook or idle `Notification` exists, yet pane liveness plus the rollup reaper reconstruct session end, and turn boundaries plus the blocking ask path plus the stall window cover the attention slice of idle. Pi and OpenCode use partial `live$`: their local stores produce session cost on turn end, while Claude and Codex have provider-native live cost sources. Pi is the no-native-ask-ui example: its one genuinely new divergence became descriptor data the shared sites consult, never a per-agent branch at a dispatch site. OpenCode is the in-process-plugin example with a native ask UI and a SQLite spend store.

The hook mapping has five jobs: route each native event to a channel; map lifecycle events to observations; render the agent's *own* decision shape (never reuse another agent's JSON — see the divergence below); put tool-name vocabularies, including blocking ask tools, in the descriptor; and set `hook_cap` from the upstream's published deadline, leaving margin so the bridge times out before the agent kills the hook.

The context read-path adds two more, either alone valid:

1. **Locate the transcript** from whatever the hook payload offers (a path, or a session id plus a discovery walk), and **map the usage record** onto raw context tokens (or a percentage) plus the cumulative total and the model, normalizing to the observation gauge fields ([the reading rules](#reading-rules)).
2. **Map the transport**, if any, onto `AgentContext` through `observe_context` — every field `Option`, tolerantly parsed, so a sparse or evolved payload always parses.

Stay best-effort throughout: a failure is an omitted field, never an error. The account and spend half of the recipe is [provider.md → Adding a provider](./provider.md#adding-a-provider).

Required tests: install/uninstall, lifecycle mapping (native event → observation → state), feed classification, coverage conformance, neutral silence, decision stdout (allow / deny / modified-input where supported), malformed-payload handling, PID attribution, version drift, and the context mapping from a fixture tail and a fixture transport payload (including the fresh-session zero and unreadable-unknown cases). Pinned stdout shapes live as inline `insta::assert_*_snapshot!(... @"...")` goldens inside each adapter's `tests` module. The adapter-authoring contract is in [`crates/rimz/src/agents/AGENTS.md`](../../../crates/rimz/src/agents/AGENTS.md).

> **Decision shapes diverge — never share one.** Claude and Codex both wrap a `PermissionRequest` answer in `hookSpecificOutput.decision`, but Codex rejects fields Claude requires (`updatedInput`, `updatedPermissions`, `interrupt`). Each adapter renders its own shape; copying one agent's JSON to another corrupts the decision.
