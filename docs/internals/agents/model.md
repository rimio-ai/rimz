# The agent model

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The per-provider native mappings (which native event means what for each agent) live in the adapter docs ([claude.md](./claude.md), [codex.md](./codex.md), [pi.md](./pi.md), [opencode.md](./opencode.md)); the account, balance, spend, and pricing model is [providers.md](./providers.md).

This doc owns how a running agent is *modeled*: the adapter boundary that produces the model's input, the fold that reduces it to one state per agent, and the live-context read path that enriches it. The model is a three-stage pipeline:

```text
adapter ── produces ──►  AgentLifecycleObservation   (one per native event)
this doc ── folds ────►  AgentState                  (one per agent)
sidebar ── projects ──►  a sidebar row
```

An adapter *produces* an [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) from each native event ([the adapter boundary](#the-adapter-boundary)); this doc *folds* those observations into one [`AgentState`](../../../crates/rimz/src/agents/state.rs) per agent; [sidebar.md](../sidebar/sidebar.md) *projects* that state into a row. The observation is agent-agnostic by construction, so everything below is too: a new agent that emits well-formed observations gets the state machine, ranking, liveness, and jump for free.

## The model at a glance

Four nouns carry the model:

- An **agent kind** is a wired integration (`claude`, `codex`, `pi`, `opencode`), described by an [`AgentDescriptor`](../../../crates/rimz/src/agents/descriptor.rs) whose `Capabilities` (`registers_lazily`, `subagents`, `background_tasks`, …) declare how that agent behaves. Every behavior below is capability-gated, so a new agent slots in by declaring what it does rather than by growing special cases.
- An **agent instance** is presence: a live local pane running a known agent right now, read from the multiplexer every tick.
- A **session** is identity: the id the agent's own hooks report, keyed `(kind, agent_id)`, where every durable fact attaches.
- The **rollup entry** is the one [`AgentState`](../../../crates/rimz/src/agents/state.rs) per session that store replay derives: the durable record the sidebar enriches and renders.

Joining instances to sessions is [the instance lifecycle](#the-instance-lifecycle); the data flow between them is:

```text
native agent event
  │  the adapter normalizes it             (the adapter boundary)
  ▼
AgentLifecycleObservation ──► one agent.lifecycle event in the store
  │  replay: reduce_agent_states folds each signal through step()
  ▼
AgentState ──► one rollup entry per (kind, agent_id)
  │  snapshot: live panes bind instances; heartbeat and
  │  sidecars refine the displayed row
  ▼
sidebar row                  (sidebar.md projects, the interface legend paints)
```

A reduced state is two axes plus one head ([`LifecycleState`](../../../crates/rimz/src/agents/lifecycle.rs)): a **status**, the running turn's **phase** ([`TurnPhase`](../../../crates/rimz/src/agents/lifecycle.rs): `reasoning`, `acting`, or `parked`, with `idle` outside a running turn), and a transient **compacting** head painted over either. The statuses, in ranking order, most attention-hungry first (so a working `running` agent settles *below* the calm `idle`/`success`):

| Status | Meaning | Decided by |
| --- | --- | --- |
| `waiting` | blocked on a human decision | the lifecycle channel: a blocking hook's `awaiting_input` signal |
| `failed` | the last turn errored | the lifecycle channel |
| `paused` | stopped mid-turn on a provider limit | derived at projection ([Displayed status](#displayed-status)) |
| `idle` | wired in, nothing in flight | the lifecycle channel |
| `success` | last turn completed cleanly | the lifecycle channel, or projection from a rollout completion marker when no `Stop` fired ([Displayed status](#displayed-status)) |
| `running` | actively working a task | the lifecycle channel |

The glyph, animation, and color for each are the canonical table in [the interface legend](../../interface/sidebar.md#reading-the-glyphs); this doc owns the transitions, not the painting.

## The rollup

[`reduce_agent_states`](../../../crates/rimz/src/store/snapshot/project.rs) folds the `agent.lifecycle` events into one `AgentState` keyed by `(kind, agent_id)`, where `agent_id` is the session id, so two concurrent agents of the same kind never share a row. Each event is a *partial* update, and how the reducer treats a field the event omits is the field's **lifetime**:

| Lifetime | Rule | Fields |
| --- | --- | --- |
| **identity** | set once when the session registers, stable thereafter | `agent_id`, `kind`, `parent_agent_id`, `agent_pid` |
| **activity** | replaced by the latest event, and *clearing* it is meaningful: an idle agent has no `task` | `status`, `task`, `last_activity` |
| **carry-forward** | persists until a newer value arrives; a missing value never resets it | `model`, `effort`, `context_pct`, `context_window`, `prompt`, `transcript_path`, `recent_prompts` |
| **live-derived** | never stored in the store; computed at snapshot time from the live pane or git | `pane`, `worktree_path`, `worktree_branch` |
| **transient heads** | opened and closed by signals, painted over the base status | the turn [phase](#turn-phase), the [compaction bracket](#the-compaction-bracket) (`compacting_since`; each close increments the durable `compaction_count`) |

[`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) and [`AgentState`](../../../crates/rimz/src/agents/state.rs) are the field catalog; the lifetimes above are the rule those types do not state. Three rules earn a note:

- A subagent's `task` is the one activity-lifetime exception: it holds the child's type (`Explore`, …) and carries forward as identity, so a finished child stays labeled when its `SubagentStop` omits the type.
- The live-derived fields follow the pane, which knows its current cwd every tick, so `worktree_path` and `worktree_branch` track a `git checkout`. Pinning them at registration would be the branch-tracking bug ([Liveness and presence](#liveness-and-presence)).
- `model` is stored **canonicalized**: a trailing capability tag is stripped (`claude-opus-4-8[1m]` → `claude-opus-4-8`). The tag rides only the fresh-launch payload; later events carry the bare id, so without canonicalization the carry-forward would flip `…[1m]` → `…` the first time a suffix-less event arrived. Canonicalizing at reduce time pins one stable label while the event log stays faithful to the raw payload.

### Instance identity and age

`last_activity` is always the agent's *own* latest event, never inherited from a previous instance of the same kind. Identity is required: a payload carrying no session id is quarantined (logged under `rimz::agent::lifecycle` and folded to nothing), so two distinct session-less instances can never merge into one row. No agent hits this today (every adapter carries a session id on its first state-bearing event), but it is where a real per-instance key would land if a future agent emitted session-less transitions.

## The state machine

An adapter emits an agent-agnostic **lifecycle signal**: the *intent* a native event carries ([`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs)). Every `agent.lifecycle` event carries its signal explicitly in the params; a payload without one is non-conforming and folds to nothing. Which native event maps to which signal is each provider's adapter doc.

One pure transition function, [`step`](../../../crates/rimz/src/agents/lifecycle.rs), folds a signal onto the prior state. It is the single home for every transition, reused identically for root agents and subagents: the reducer calls it on replay to derive the rollup, and the hook-ingestion path calls it once per fresh event to log any anomaly. Both read the same table, so the two can never disagree.

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
 waiting    : awaiting input enters from any status; the next turn, tool,
              or compaction close returns the row to running
 removed    : session ended · pane reverted to a shell · reaped (no row)
```

The edges, precisely:

| Signal | From → to | Note |
| --- | --- | --- |
| `registered` | *(none)* → `idle` | establishes the row; with `subagent_started`, the only signal that does |
| `turn_started` | any → `running` | opens the turn in the `reasoning` phase and stamps a fresh prompt boundary; a parked running row resumes and carries the prior boundary |
| `turn_ended`, clean | `running` → `success` | the turn resolved; the phase rests |
| `turn_ended`, errored | `running` → `failed` | the error bit always wins |
| `turn_ended`, clean with background work in flight | `running` → `running` | the main thread parked, the phase is `parked`; see below |
| `awaiting_input` | any → `waiting` | a blocking prompt ([`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs): permission, plan approval, or question) holds the row for a human; a repeat restamps it |
| `subagent_started` | *(none)* → `running` | establishes the child row, keyed by the child's own id |
| `subagent_stopped` | `running` → `success` / `failed` | the child's terminal verdict, kept through the parent's turn |
| `tool_used` (mutating) | resting or *(none)* → `running`, reconciled; `waiting` → `running` | completed work proves a turn; attention rows hold; a tool on a waiting row is the answered-permission edge; the first file-editing tool moves the phase to `acting` |
| `compacting` | status and phase held | stamps the [compaction head](#the-compaction-bracket); a waiting row stays waiting |
| `compaction_ended` | auto → `running` (phase carried) · manual → `idle` · trigger unknown → held · any close on `waiting` → `running` | closes and counts an open [bracket](#the-compaction-bracket) |
| `ended` | removal | the reducer's tombstone path handles it upstream; reaching `step` it is an ignored no-op |
| `lost` | held | Legacy `rimz.agent-lost` marker retained for log replay compatibility, reaching `step` as an ignored no-op |

A `TurnEnded` resolves the turn to `success`, or `failed` on its error bit, never back to `idle`. One exception keeps it `running`: a clean end that also carries `parked_on_background` means the main thread *parked on still-in-flight background work* rather than finishing, so the row stays `running` in the `parked` phase and paints a distinct `⋯ bg` marker rather than a false `✓` (the provider-specific detection is in [claude.md](./claude.md#hooks-and-lifecycle)). A parked row quiet past the stall window settles to `success`; reawakened activity advances its heartbeat and makes the row `running` again. Claude wakes a parked parent by injecting the finished background task's notification as a `UserPromptSubmit`: folded onto a parked running row, that `TurnStarted` resumes the same logical turn and carries `turn_started_at` forward, so child verdicts stay visible through the delegation wave. Once the turn reaches a clean end, the next prompt stamps fresh and clears past-turn verdicts.

A `SubagentStopped` resolves the *child* the same way (`success`, or `failed` on its error bit), and the sidebar keeps that `✓`/`!` through the parent's turn ([sidebar.md → Sub-agent lists](../sidebar/sidebar.md#sub-agent-lists)).

`waiting` arrives like every other status: a blocking hook (permission request, plan approval, user question) classifies as [awaiting-user](#two-hook-channels) and records an `awaiting_input` lifecycle signal, and `step` moves the row to `waiting`. The reducer stamps `waiting_since` from the signal, and the shared guard [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs) (`status == waiting && last_activity <= waiting_since`) is the single authority read paths use, so an activity heartbeat that postdates the ask releases an answered sub-turn prompt without waiting for a durable clear. A provider interruption marker newer than `last_activity` also releases a waiting row to `idle`, proving Esc cancelled the native prompt when no lifecycle hook reports the cancellation. A transition off `waiting` sets `waiting_cleared`, and the ingestion path appends those durably even for signals it would otherwise skip, so a non-mutating approved tool still clears the row on replay. `paused` is derived at projection ([Displayed status](#displayed-status)).

**Fail-soft, never silent.** `step` is total: an unexpected `(state, signal)` pair never panics and never freezes. It takes the signal's natural edge (the agent is authoritative about its own activity) and tags the result [`TransitionKind::Reconciled`](../../../crates/rimz/src/agents/lifecycle.rs) with the state it overrode and why. The reducer discards the tag; the ingestion path ([`cli/hooks.rs`](../../../crates/rimz/src/cli/hooks.rs)) logs it once per fresh event under `rimz::agent::lifecycle` to stderr (`warn!` on a reconciled edge, `debug!` on an ignored no-op, `error!` on a quarantined identity), keeping hook stdout for the decision channel. Drift between the model and reality leaves a structured breadcrumb instead of a wrong-but-quiet row. The headline case is in the edge table: a **tool observed on a resting row** proves the rollup is stale, so `step` moves it to `running` and logs the edge.

**Extending the signal vocabulary.** A new provider-observed [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs) variant requires both (a) a concrete native event on a shipping provider that no existing variant plus enrichment expresses, and (b) a distinct `(status, phase)` edge in `step`, landed with its edge test and the totality test extended. RimZ-synthesized side-channel markers like `Lost` are narrower: they need replay compatibility semantics and an explicit ignored edge. `CompactionEnded` is the worked example: three providers close the bracket with different evidence and one optional trigger bit, and the same signal owns all three edges. Anything short of both is enrichment on an existing signal: Pi's `stopReason: "aborted"` rides `TurnEnded { errored: true }`, and a `Verifying` phase has no provider that emits one.

### Turn phase

The phase is the running turn's shape: the agent owns its status, and RimZ derives the phase from the turn's own hook events. Every turn opens in `reasoning` (`TurnStarted` and `SubagentStarted` set it), and the sidebar paints the thinking head while the turn reads, searches, and decides. The turn's first **file-editing** tool moves it to `acting` (`ToolUsed { edits: true }`, each adapter's file-writing subset read through `tool_edits_files`). The trigger is always a hook event, never prompt or transcript content.

```text
turn starts ──► reasoning  ──first file-editing tool──► acting ──► turn ends
                    │                                                  ▲
                    └── a research turn that never edits a file ───────┘
clean end with background work still in flight ──► parked (the row stays running, ⋯ bg)
```

- **A research turn stays in the thinking head end to end**: searches, reads, and shell commands write no file, so a turn that answers without editing stays in `reasoning`.
- **A shell command is work without writing**: it keeps the row live and leaves the phase in place. A phase that left `reasoning` never re-arms mid-turn.
- **Any turn boundary rests the phase**: `TurnEnded` and `SubagentStopped` drop it; the next prompt re-arms it. A clean end with background work still in flight parks it instead.
- **Subagents** own separate `agent_id`s, so a child observation never mutates its parent's phase. The lifecycle channel is bracket-grained for children: only `SubagentStarted`/`SubagentStopped` fold to the child's rollup, a child's per-tool events are dropped at the adapter, and the sidebar's child entry carries status only.

A quiet `parked` row settles to `success` through the stall rung, while silent `reasoning` and `acting` rows escalate as stalled. The phase vocabulary is painted once, in [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

### The compaction bracket

Compaction is a transient head over the status. The opening signal (`Compacting`) stamps `compacting_since` and holds the prior status and phase, so the sidebar pulses the compaction head over whatever the agent was doing. The session's next lifecycle signal closes the bracket: `step` emits the close as a transition fact ([`Transition::compaction_closed`](../../../crates/rimz/src/agents/lifecycle.rs)), and the rollup increments the durable `compaction_count` from it exactly once per bracket. The card surfaces the count as `↻ N` on the context line.

`CompactionEnded` is the explicit close, and its trigger decides where the agent lands: a known **automatic** trigger returns to `running` with the interrupted phase carried (automatic compaction happens mid-turn); a known **manual** trigger rests to `idle` (`/compact` runs between turns); an **absent** trigger holds the prior status and phase. A close that rests the agent advances `turn_started_at`, retiring the prior turn's subagents the same as a fresh prompt or `/clear`; an automatic mid-turn close resumes the turn and holds the boundary. Redundant close signals are idempotent, since an absent bracket closes nothing. The projection also expires the head past a short display window, so a crash mid-compact can never pulse it forever.

A compaction signal for a session the rollup has never seen folds to nothing. Codex compaction rotates thread ids before the replacement session is real, so the rotated id registers at its first turn or tool signal instead; aborted compactions cannot create unreapable ghosts that steal pane primacy.

## Displayed status

`snapshot.agents` (the rollup as `rimz sidebar snapshot` reports it) keeps the agent-owned truth. Read paths share a cheap [`effective_status`](../../../crates/rimz/src/agents/state.rs) projection, so a still-`running` turn with an active provider-park marker reads as `paused`, and a hookless completion or interruption reads as `success` or `idle`, in `rimz pane list` and message delivery; the sidebar row projection then adds budget-aware refinements on top. The refinements are one family with a pinned precedence, top rung wins:

1. **A human-blocked `waiting` row stays first.** An open blocking prompt outranks every refinement below unless a newer turn-interruption marker proves Esc cancelled it; that row settles to `idle`. A `waiting` row that otherwise fails [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs) (activity postdating the ask proves it was answered in the pane) projects back to `running` in the `reasoning` phase until a durable clear lands.
2. **`paused`**: an agent whose latest turn stopped on a provider limit, a RimZ dollar cap, or a transient API error. No hook emits `paused`; RimZ derives it at projection, and it joins the cockpit tally just under the actionable attention states. The marker can refine a still-`running` row (the provider emitted no lifecycle end) or a same-turn `failed` row (the lifecycle recorded an errored end). Provider `rate_limit` and `spend_limit` certificates are per-agent while their budget decision is account-scoped. A launch `budget` instead reads the session's cumulative live cost and a per-session runtime ledger; crossing it sends Esc and stamps the row with spend against cap. `/day` ledgers rebase at the configured local day and arm auto-continue for that boundary, while absolute parks stay put until raised, cleared, or waived for one human-started turn. `overloaded` covers provider overload, serving capacity, 5xx-class failures, stalled streams, timeouts, and connection drops; it holds until a newer hook event self-clears it ([provider.md → Spent windows](./providers.md#spent-windows-and-paused-rows)).
3. **Waiting on children**: a `running` agent with a live subagent paints a quiet wave, exempt from the stall escalation. The stall clock reads the row's displayed activity, which folds in the children's, so a child that just finished defers the escalation too.
4. **Turn death**: a non-transient provider API error or unclassified turn-death marker escalates to `!` at once. For a still-`running` row the marker postdates `last_activity`, so the explicit death certificate beats the stall window; for a terminal `failed` row the marker must fall inside the row's current turn, so an old marker never explains a fresh failure. Transient 5xx, stall, timeout, and connection errors park like overloads once the marker proves that class. The card quotes the upstream or derived error label, and any newer hook event self-clears it.
5. **Turn completion**: a `running` row whose latest turn finished without a `Stop` hook settles to `success`. Codex's `/review` ends on a clean rollout `task_complete` with a non-empty `last_agent_message` and no `Stop`, so the completion marker postdates `last_activity` and settles the row instead of letting the stall window misread a finished review as failed ([codex.md → Turn-completion marker](./codex.md#turn-completion-marker)). A turn-death marker outranks it; a newer prompt self-clears it.
6. **Turn interruption**: a `running` row whose latest turn was aborted without a `Stop` hook, or a `waiting` row whose native ask was Esc-cancelled, settles to `idle`. The marker source is provider-specific: Codex writes rollout `turn_aborted` for Esc and `/clear` mid-turn ([codex.md → Turn-interruption marker](./codex.md#turn-interruption-marker)); Claude writes a transcript `user` sentinel beginning `[Request interrupted by user` ([claude.md → Turn-interruption marker](./claude.md#turn-interruption-marker)). The marker postdates `last_activity` and settles the row as at rest with no result instead of letting a false wait persist or the stall window misread it as failed. A turn-death marker outranks it; a newer prompt self-clears it.
7. **Stall**: a `running` agent silent past the configurable stall window settles to `success` when its phase is `parked`, projects to `paused` when its kind has a spent, unreset window, and otherwise escalates to the attention `!` (see [Liveness and presence](#liveness-and-presence)).

Each rung reads enrichment plus liveness, and each leaves `snapshot.agents` holding the true lifecycle status: Claude transcript-death can leave the rollup `running` while Codex Stop-over-rollout-error records the rollup `failed`, and projection refines either display to `paused`. The order is a pinned contract. The [`displayed_status_precedence_ladder_holds`](../../../crates/rimz/src/store/snapshot/view/tests/status/stall.rs) test stacks the error/stall causes, and the `turn_complete`/`turn_interrupted` status tests pin the settle rungs, so a reordering fails the suite even when every single-cause test still passes. The phase and head paints ride over this base: a `running` agent in `reasoning` renders the thinking head, and an open compaction bracket pulses over any base status.

## The instance lifecycle

An agent reaches the sidebar as an **agent instance**: a live local pane running a known agent command or hosting one live agent CLI under its pane root, bound one-to-one to its pane id, `pane_pid`, and process-start. A **session** binds to it, and the instance exits when its pane reverts to a shell. The instance exists before any session id is known, and the lifecycle's one hard problem is joining the two. The join turns on two independent axes: how the hook reports its identity, and where the agent runs.

**Hook identity: stamped or daemon-routed.** A **standalone** agent runs in its pane, so the hook is a descendant of it and reads the pane env and pid directly: it **stamps** the pane id onto the session. A **daemon-routed** agent runs through a background daemon, so the hook fires from the *daemon* (no pane env, the daemon's shared pid) and the session is **unstamped**. Claude is standalone; Codex 0.137+ daemon-routes its hooks through the shared app-server, so in-pane Codex sessions are unstamped and bind through the recovery ladder below ([codex.md](./codex.md#session-registration-and-launch-quirks)); Pi and interactive OpenCode run in-process in the pane and are standalone.

**Presence: in-pane or remote.** Orthogonal to hook identity is where the agent actually runs:

- **In-pane.** A local pane runs the agent, with its own `pane_pid`, so the user can jump to it. A standalone agent binds its pane by the stamped id; a daemon-routed *in-pane* agent (a Codex CLI thin-wrapping the daemon) is unstamped and binds through the recovery ladder. Either way it renders as a normal, jump-able row.
- **Remote.** The agent runs only in the daemon, with no local pane: `claude remote-control --spawn worktree`, or a Codex thread started from the web. It carries a worktree but nothing to focus. **RimZ does not render remote agents yet: a documented gap, deferred to a future round ([sidebar.md → Presence model](../sidebar/sidebar.md#presence-model)).** The `claude remote-control` host pane itself is separate infrastructure, filtered out of the room and surfaced as the `⇅ rc` flag.

So the binding test is one question: does a live local pane bind the session? A stamped session binds by id; an unstamped session binds through the recovery ladder; a session no pane binds is a remote agent.

**Phase 1: pre-session presence.** A wired instance with no bound session yet renders as an idle agent row, so a just-launched agent reads as itself rather than a bare process. Claude reaches this at the login screen and in the short span before `SessionStart` stamps the pane; Codex and OpenCode reach it before their first real session exists. RimZ synthesizes an idle `○ <kind>` row until a lifecycle hook binds the real session ([`idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). The gate is wired hooks: an installed integration can later report status, and an unwired instance stays a [process row](../sidebar/sidebar.md#process-rows). The descriptor's `registers_lazily` flag is for cwd session binding, not idle synthesis.

**Phase 2: session binding.** A lifecycle hook arrives carrying a session id, and RimZ joins it to the right instance. A standalone hook stamped the pane id, so the join is exact and free. An unstamped session walks a deterministic recovery ladder: hook ingestion writes a recovered same-cwd pane stamp from the repaired live frame; a `codex resume <session-id>` pane binds exactly; then same-cwd sessions pair newest-first to the latest viable pane process-start before the session's first event. Residual ambiguity binds deterministically and appends a `binding.log.jsonl` breadcrumb. The ladder's guards and limits are [sidebar.md → Presence model](../sidebar/sidebar.md#presence-model).

**Phase 3: instance exit.** The in-pane agent process is the liveness truth, surfaced through the pane: the CLI client is the pane's foreground process or the single hosted descendant under the pane root, so when it exits the pane reverts to a shell and stops reading as an agent. The instance leaves with no exit hook, in both launch modes. A `SessionEnd` hook (Claude) tombstones the session eagerly on top of this, clearing its row and context sidecar at once; Codex has no `SessionEnd`, so a Codex session leaves by pane liveness and the [rollup reaper](#liveness-and-presence) alone.

Daemon-routed Codex hooks first name the shared app-server daemon, then the recovery ladder re-owns any local session to its in-pane CLI process and stores the full pane stamp (`pane_id`, tab id, cwd, pane pid, and process start). An unbound daemon-owned session abstains from pid liveness and ages through the ghost TTL like a pidless row; the app-server loaded-thread reaper remains a faster secondary signal, dropping a daemon-mode session absent from `thread/loaded/list` before the pane fold ([`reap_runtime`](../../../crates/rimz/src/store/snapshot/view/reap.rs)) while an unreachable daemon or untrusted list keeps every session.

## Liveness and presence

Presence comes from the live pane, with no exit event required: an agent renders only on the pane it stamped, and one whose pane reverts to a shell or closes is gone on the next snapshot. **The binding mechanics (stamped pane id, the Codex daemon exception, jump reconciliation) live in [sidebar.md → Presence model](../sidebar/sidebar.md#presence-model); this section owns only what the rollup contributes.** There is no `offline` status: a dead agent is a reverted shell row or no row, never a retracted store fact.

**Stamped-pane binding decides what renders; the captured pid feeds the reaper.** RimZ records the pid best-effort on each lifecycle event (`RIMZ_AGENT_PID=$PPID`, falling back to a process ancestor walk, plus a platform process-start token to defeat pid reuse), and the reaper reads *pidless* as one ghost signal. Stamped-pane binding already keeps a stale agent off a stranger's pane, so the pid never gates rendering.

**Per-tool activity rides a runtime heartbeat.** The durable event log is turn-grained, so `last_activity` would otherwise advance only at turn boundaries. The hook touches a per-agent heartbeat ([`agent_activity`](../../../crates/rimz/src/agent_activity.rs)) on every progress-proving event (each completed tool call, the turn boundaries, subagent start/stop), and the snapshot folds the freshest touch into `last_activity`. A pre-tool event or a blocked wait touches nothing, and the heartbeat is keyed by the event's own session: a backgrounded subagent's progress touches the *child's* heartbeat, and a parent blocked on an ask keeps its `last_activity` frozen until it acts. The signal does three things:

- It keeps a busy agent's row animating.
- It escalates a `running` agent silent past the configurable stall window (30 minutes by default) to the `!` attention state.
- It recovers an answered ask: once `last_activity` passes `waiting_since`, [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs) reads false, so an agent whose prompt was answered in its own UI returns to `running` without waiting for the next turn boundary.

Like every heartbeat it is latency, not truth: a missing file just leaves `last_activity` at the event-log timestamp.

**Session death converges to the durable log.** After a publishing commit, the debounced write-path reaper appends an `Ended` tombstone for every root session whose death is provable from the store plus the process table: a recorded owner is dead, a pidless or daemon-owned session is inactive past the ghost TTL, an older session was replaced by a different process in the same pane or by a newer paneless remnant in the same worktree, or a newer fresh-lineage conversation supersedes it after `/clear` or `/new`. The persisted live roster protects crash-recovery candidates until rebirth planning consumes the roster. Runtime expel and the snapshot-time view reap apply the same liveness and shared supersession rules as latency shims during the debounce window; the daemon loaded-thread reap stays snapshot-side because its websocket probe is live external input. An agent holding its own distinct pane is kept, and subagents leave transitively with their parent. A new death signal appends a durable `Ended` observation unless it explicitly depends on live external input. This workspace-local convergence complements the cross-workspace `rimz gc`.

## The adapter boundary

A coding agent reports to RimZ through hooks, and every agent speaks through one trait, [`AgentAdapter`](../../../crates/rimz/src/agents/mod.rs). [`registry::all_adapters`](../../../crates/rimz/src/agents/registry.rs) chains the compiled-in `ADAPTERS` slice with validated machine-tier process plugins. The trait is the single place a native protocol diverges and the single place it is normalized; nothing downstream of it is agent-specific. The per-provider mappings it produces are the adapter docs; the raw upstream protocols they read are the [external references](../../externals/agent-adapter/claude-reference.md), and the external process wire is [plugin.md](./plugin.md).

An agent reports through the same public shape everything else uses: a hook is an adapter that translates a native protocol onto one RimZ CLI entrypoint, and the observations it records land in the same store every read surface projects.

### The seam: `AgentAdapter`

Built-in adapters implement the trait plus a static [`AgentDescriptor`](../../../crates/rimz/src/agents/descriptor.rs) (identity, branding, capabilities, tool tables, integration coverage) and one registry line. External plugins use the shared `PluginAdapter`, which builds the same descriptor and behavior from a validated manifest and canonical envelope. The methods, by role (signatures live in the trait):

- **`classify_hook`** sorts a native event into one of the two channels below (or `Unknown`, dropped) and, for a blocking event, names the [`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs).
- **`observe_lifecycle`** is the normalizer: it maps a native lifecycle event onto one [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs). `None` means "no transition here", so high-frequency events stay silent.
- **`render_neutral`** emits the agent-native no-op for blocking asks. Hooks record the waiting observation and return neutral; the agent's own UI stays open as the answer surface.
- **`observe_context`** normalizes a rich out-of-band payload into [`AgentContext`](../../../crates/rimz/src/agents/context.rs); **`local_context_refresh`** derives sidecar fields from local provider state on hook or producer tick triggers; **`context_refresh_spawn`** maps hook or tick triggers to a detached `rimz` helper when a provider's rich context transport needs one.
- **`install_hooks`** / **`uninstall_hooks`** / **`hooks_installed`** own the per-user config write and report it; **`probe_account`** / **`parse_spend`** / **`transcript_files`** feed the account and spend model in [providers.md](./providers.md).

Two invariants hold the seam shut:

- **Adapters never touch the store.** The adapter is a pure mapper. [`rimz hooks feed`](../../../crates/rimz/src/cli/hooks.rs) owns every store write; it calls the adapter for classification and neutral output only.
- **Nothing downstream reads a native payload.** The adapter emits exactly two things the rest of RimZ consumes: an `AgentLifecycleObservation` and a blocking-ask classification. A native field reached for outside an adapter is a mapping that belongs *in* the adapter.

### Two hook channels

`classify_hook` sorts every native event into one of two wired channels. The distinction is whether the hook can hold the agent open while RimZ waits for an answer.

**Lifecycle: fast, non-blocking.** Drives agent status, the turn phase, task, and enrichment. Each flows through `observe_lifecycle`; an event carrying no transition returns `None` and records nothing.

**Awaiting-user: records the waiting state and returns neutral.** A permission request, plan approval, or user question ([`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs)) becomes an `awaiting_input` lifecycle signal when the agent has its own ask UI. RimZ records the signal — the row goes `waiting`, and the ask's question text lands as a transcript `Ask` entry — returns the agent-native no-op immediately, and leaves the prompt visible in the agent's pane. An agent whose descriptor declares `native_ask_ui` off (pi) gets the same neutral no-op with **no waiting observation**, since there is no native prompt a `?` row could route the human to.

Blocking decision hooks must be **sync**: an async one would ignore the decision printed on stdout, so the installer rejects it.

### Hook stdout is the decision channel

This is the canonical statement of the rule the rest of the docs link to. A hook's stdout carries only the agent-native neutral no-op for blocking asks, and the agent's own UI stays responsible for the decision. It follows that:

- **Logs never go to stdout.** They go to stderr or RimZ runtime state logs such as `binding.log.jsonl` (the `print_stdout` lint gates this; see [rust-conventions.md](../../contributing/rust-conventions.md)).
- **Hook helper children get fresh, fully-piped stdio, never inherited.** A wrapped statusline command's stderr or a notification helper's chatter must never leak onto the decision channel; a CI grep rejects `Stdio::inherit` in hook paths.
- **Every neutral shape is golden-tested**, including the agent-native no-op.

### Hooks resolve the room they live in

A hook resolves its workspace as a **participant** ([`WorkspaceResolver::resolve_participant`](../../../crates/rimz/src/workspace.rs)): the session's identity pin (`RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT`, stamped into the mux environment at birth) wins over re-deriving identity from cwd, so an agent working inside a nested repo in a directory room writes to the room its pane lives in. The pin is hash-verified and any mismatch falls through to the static ladder (git → marker → directory): a hook on the agent's critical path degrades on identity, never errors. Every participant surface resolves the same way, and a CI grep keeps the create-mode resolver out of them; room-choosing commands resolve statically, so a deliberate per-repo room can still be created from inside a parent room.

A **daemon-routed** hook (Codex's, fired from the shared app-server) inherits its daemon's environment, not the pane's, so the env pin never reaches it. `rimz hooks feed` recovers the pin from the sibling agent process instead ([`resolve_participant_with_pin_recovery`](../../../crates/rimz/src/workspace.rs)): the daemon spawns hooks with the session cwd, so the in-pane agent process sharing that cwd carries the pane's pin in its environment. Each candidate is verified like the env pin and adopted only when every candidate names one root; a split scan or unsupported host degrades to the static ladder. The full order: `--root`, env pin, recovered sibling pin, static ladder.

### From native event to internals

A lifecycle hook fires → `classify_hook` returns `Lifecycle` → `observe_lifecycle` maps the payload onto an `AgentLifecycleObservation` → the CLI records it as an `agent.lifecycle` event, and [the rollup](#the-rollup) and [the state machine](#the-state-machine) above own it from there.

A blocking hook fires → `classify_hook` returns `AwaitingUser` with an `AskKind` → the CLI records the `awaiting_input` lifecycle event when the adapter declares a native ask UI, calls `render_neutral`, and exits. The agent's UI owns the prompt; the sidebar's `?` row routes you to the pane, and you answer there.

### Hook install: the visible security step

Installing hooks edits the agent's own config, so it is a security surface, never silent. `rimz start` detects installed, supported agents each run, prints one consent prompt covering every missing agent (config path, additive impact, undo command, and a hook-command example), installs all listed agents on Enter, and installs nothing on `n` or EOF. `rimz hooks install --dry-run` prints the unified diff without writing; `rimz hooks install` installs every detected supported agent on PATH; `rimz hooks uninstall` removes every RimZ-managed hook set even when the agent binary is gone. `hooks_installed()` makes the state observable: `rimz doctor` reports it per agent. An agent run before its hooks are installed fires nothing and is invisible, never silently broken.

**What install wires.** Every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment and audit depth current. The single source of truth for the wired set is each adapter's `INSTALLED_EVENTS`-style constant, not restated here. Install detection requires the full canonical set, so an under-wired config re-offers the idempotent merge. Per-tool payload *content* is gated by `[privacy] payload_mode` ([configuration.md](../../guide/configuration.md#sidecars-and-privacy)); the gate strips content, never whether a transition is observed.

**The installed config shape.** Claude and Codex have no wildcard event key, so install writes one block per wired event; Pi and OpenCode instead own one whole integration file (see [pi.md](./pi.md), [opencode.md](./opencode.md)). Inside that shape it stays minimal:

- **One command for every event**: `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`, with no `--event`. The helper reads the event from the payload's `hook_event_name`.
- **Idempotent, self-healing reclaim.** Install reclaims every rimz-owned entry by the stable command substring `rimz hooks feed --source <agent>`, then rewrites the canonical set, so duplicate or stale blocks never accumulate. User-authored hooks are untouched.

**Trust.** Every hook command enters the executable-surface hash, so a tampered hook config demotes project trust to stale (see [trust.md](../harness/trust.md)).

## Enrichment

The store and explicit events decide routing, ranking, and state; enrichment paints the row. `task`, `context_pct`, `context_window`, and `total_tokens` are **enrichment**: display-only and redactable. A missing value means "the agent didn't report it," never zero. The sidebar still paints a context bar for every observed agent, drawing an unreported gauge at a visible 0% baseline.

`context_window` is the model's window in tokens, and uniformly across agents it is the model's max **input** tokens: the gauge numerator counts input-side occupancy only (`input + cache`, never output; see [`context_used_tokens`](../../../crates/rimz/src/agents/state.rs)), so a model that splits its window into separate input and output caps scales against the input cap. Each adapter resolves the window its own way (Claude from the payload model id, where `[1m]` widens it; Codex from the rollout's `model_context_window`; OpenCode from its model catalog), and the card's identity line renders it (`258k`, `1M`), preferring the fresher out-of-band reading from [`AgentContext`](#rich-context-agentcontext) when one exists.

Context budget is the one field no agent puts in its hook JSON; usage lives in the transcript or in a provider-owned in-process gauge (the [two sources](#two-sources) below). These are bare token counts; `payload_mode` gates the *content* of high-frequency payloads, never these gauges.

### Two sources

A session's context data has two origins. Both flow through the [`AgentAdapter`](../../../crates/rimz/src/agents/mod.rs) and normalize onto the same internal fields, so a new provider implements one or both and the rest of RimZ is unchanged. Enrichment is **never correctness**: a missing file, a torn line, or an absent agent each degrades to an omitted field, never a failed hook or a wrong decision.

**The transcript/store read path** is the universal floor. Every provider has a local usage store the spend parser already understands (JSONL for Claude, Codex, and Pi; SQLite for OpenCode), so the row can derive a session dollar total from the same `parse_spend` path that feeds history. For Claude this stays a low-frequency fallback because statusline owns the live `AgentContext`. For Codex the rollout tail is also the native live token/cost/effort source: progress hooks and the elected snapshot producer run a stat-gated local refresh that reads a bounded tail only when `(mtime, nanos, len)` changes. For Pi and OpenCode, a turn-ended signal resolves the current session store, sums that session's spend entries, and declares `live$` as partial coverage because the figure is reconstructed on turn end rather than provider-pushed.

**The rich-context transport** is the provider-specific upgrade, where a provider offers one. It carries everything the local read cannot derive (rate-limit windows, account plan, PR info, model display name, version) on that provider's own cadence. Each transport differs and is documented in its adapter doc (Claude pushes statusline JSON, Codex reads read-only app-server methods); transport payloads normalize through `observe_context` into one [`AgentContext`](../../../crates/rimz/src/agents/context.rs), while local transcript refreshes use `local_context_refresh(RefreshTrigger::Hook|Tick)` and detached rich refreshes use `context_refresh_spawn` through the same trigger model.

A provider whose hook wire RimZ authors has a third option: stamp the gauge **onto the hook payload itself**. Pi's extension does this on every envelope from its in-process context API, and OpenCode's plugin maintains its gauge from `message.updated` events and stamps the latest split, plus the model's context window from OpenCode's own catalog, onto each lifecycle envelope. Neither then needs a transcript tail or a general transport.

### Reading rules

The tail reader is provider-agnostic ([`read_transcript_tail`](../../../crates/rimz/src/agents/transcript_fs.rs)) and every adapter parses on top of it under the same rules:

- **Bounded.** Read at most the trailing 64 KB, so a multi-megabyte log never stalls a hook.
- **Newest-first.** Scan lines in reverse and take the most recent usage record; a truncated leading line from the seek simply fails to parse and is skipped.
- **Stop when found.** Bail as soon as the needed records are in hand.
- **Lossy and forgiving.** Decode as lossy UTF-8; any IO or parse failure yields empty fields.
- **Zero vs unknown.** A transcript that opens cleanly but carries no usage yet is a *fresh* session: report an explicit `0%` so the bar draws empty rather than vanishing. A transcript that cannot be read stays unknown (`None`): "the agent did not report it," never a false zero.

### Rich context (`AgentContext`)

Some agents publish far richer per-session data out of band than their hooks carry: context-window accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, thread preview, PR info, version, effort. The *transport* differs per agent and lives in its adapter doc; `observe_context` normalizes transport payloads into the agent-agnostic [`AgentContext`](../../../crates/rimz/src/agents/context.rs). Every field is `Option` and tolerantly parsed, so a sparse or evolved payload always parses and the renderer draws whatever subset is present. The account/balance subset (plan, metered, rate-limit windows) folds into the provider dashboard; its mapping and aggregation are [providers.md](./providers.md).

This is high-frequency, display-only enrichment, so it does **not** ride the event log. RimZ writes a **latest-wins per-session sidecar**, one atomic file per `(kind, agent_id)` under the runtime `agent_context/` dir, from CLI producer paths (statusline feed, hook ingestion, detached refresh helpers, the Codex stat-gated backstop). `rimz sidebar snapshot` folds each record onto its `AgentState`.

The sidecar lives wholly off the durable path (store first; sidebar wakeups are latency, not truth) and dies with the session: a session-end event tombstones it, a missed tombstone becomes invisible when the rollup row is reaped and the snapshot join has no surviving row to enrich, and `rimz gc` sweeps old files. The file sits under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

## Adding an agent

Claude, Codex, Pi, and OpenCode are compiled-in integrations because Rimz owns their hook installers and provider-specific enrichment. A third-party agent normally ships a [process plugin](../../reference/agent-plugins.md): one machine-tier manifest, native shim, and optional probes, with no Rimz source change. A new built-in remains appropriate when Rimz must own a native config migration or a protocol surface that the canonical wire cannot express; it lands as one directory under [`crates/rimz/src/agents/`](../../../crates/rimz/src/agents/AGENTS.md), one `registry::ADAPTERS` line, conformance coverage, and its adapter doc.

The descriptor carries two declared matrices, both conformance-checked and both printed by `rimz coverage` (wired green, partial yellow, unsupported/absent dim, so absences are visible at a glance):

- The **`coverage`** table declares every `IntegrationConcern` as `Wired { via }`, `Partial { via, gap }` (no native signal, the behaviour reconstructed by derivation, the gap named), or `Unsupported { reason }`. Codex and OpenCode use partial `end` and `idle`: no per-session end or idle hook exists, yet pane liveness plus the reaper reconstruct end, and turn boundaries plus the ask path plus the stall window cover the attention slice of idle. Pi and OpenCode use partial `live$`.
- The **`lifecycle_hooks`** table declares every `LifecycleSignalKind` as `Native { event }`, `Derived { via, gap }`, or `Absent { reason }`.

The hook mapping has four jobs: route each native event to a channel, map lifecycle events to observations, render the agent's neutral no-op, and put tool-name vocabularies in the descriptor. The context read path adds two more; either alone is valid:

1. **Locate the transcript** from whatever the hook payload offers, and **map the usage record** onto raw context tokens, the cumulative total, and the model, normalizing to the observation gauge fields ([the reading rules](#reading-rules)).
2. **Map the transport**, if any, onto `AgentContext` through `observe_context`: every field `Option`, tolerantly parsed.

Stay best-effort throughout: a failure is an omitted field, never an error. The account and spend half of the recipe is [provider.md → Adding a provider](./providers.md#adding-a-provider).

Required tests: install/uninstall, lifecycle mapping (native event → observation → state), ask classification, coverage conformance, neutral silence, malformed-payload handling, PID attribution, install version drift, and the context mapping from a fixture tail and a fixture transport payload (including the fresh-session zero and unreadable-unknown cases). Pinned stdout shapes live as inline `insta` goldens in each adapter's `tests` module. The adapter-authoring contract is in [`crates/rimz/src/agents/AGENTS.md`](../../../crates/rimz/src/agents/AGENTS.md).

> **Neutral semantics diverge; verify per agent.** Empty stdout hands the prompt to Claude's and Codex's own UI, but for Pi — which has no native prompt to fall back to — empty stdout *is* the allow. Each adapter documents what its no-op means; never assume one agent's neutral behaviour for another.
