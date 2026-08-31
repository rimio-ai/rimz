# The agent model

A coding agent runs in a pane, reports through its own hooks, and appears in the sidebar as one card. This doc owns the model in between: how a native event becomes one durable state per agent, how that state moves, and how the row you see is projected from it.

The pipeline has three stages, each owned by a different doc:

```text
adapter ── produces ──►  AgentLifecycleObservation   (one per native event)   adapter.md
this doc ── folds ────►  AgentState                  (one per agent)          model.md
sidebar ── projects ──►  a sidebar row                                        sidebar.md
```

An adapter produces an [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) from each native event ([adapter.md](./adapter.md)); this doc folds those observations into one [`AgentState`](../../../crates/rimz/src/agents/state.rs) per agent; [sidebar.md](../sidebar/sidebar.md) projects that state into a row. The observation is agent-agnostic by construction, so everything below is too: a new agent that emits well-formed observations gets the state machine, ranking, liveness, and attention routing for free.

The commitments this operationalizes are in [DESIGN.md](../../../DESIGN.md). Which native event means what for each agent is that agent's own page ([adapter_claude.md](./adapter_claude.md), [adapter_codex.md](./adapter_codex.md), and eleven siblings); the account, balance, spend, and pricing model is [providers.md](./providers.md).

## Four nouns

- An **agent kind** is a wired integration (`claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `cursor`, `droid`, `kiro`, `qwen`, `grok`), described by an [`AgentSpec`](../../../crates/rimz/src/agents/definition.rs).
- An **agent instance** is presence: a live local pane running a known agent right now, read from the multiplexer every tick.
- A **session** is identity: the id the agent's own hooks report, keyed `(kind, agent_id)`, where every durable fact attaches.
- The **rollup entry** is the one `AgentState` per session that store replay derives: the durable record the sidebar enriches and renders.

Joining instances to sessions is [the instance lifecycle](#the-instance-lifecycle). The data flow between them:

```text
native agent event
  │  the adapter normalizes it                        (adapter.md)
  ▼
AgentLifecycleObservation ──► one Store lifecycle transaction
  │  replay: reduce_agent_states folds each signal through step()
  ▼
AgentState ──► one rollup entry per (kind, agent_id)
  │  snapshot: live panes bind instances; the heartbeat and
  │  sidecars refine the displayed row
  ▼
sidebar row                  (sidebar.md projects, the interface legend paints)
```

## Status and phase

A reduced state is two axes plus one head ([`LifecycleState`](../../../crates/rimz/src/agents/lifecycle.rs)): a **status**, the running turn's **phase**, and a transient **compacting** head painted over either.

The statuses, in ranking order, most attention-hungry first ([`status_weight`](../../../crates/rimz/src/store/snapshot/view/score.rs)):

| Status | Weight | Meaning | Decided by |
| --- | --- | --- | --- |
| `waiting` | 600 | blocked on a human decision | the lifecycle channel: a blocking hook's `awaiting_input` signal |
| `failed` | 560 | the last turn errored | the lifecycle channel |
| `paused` | 400 | stopped mid-turn on a provider limit | derived at projection ([Displayed status](#displayed-status)) |
| `success` | 300 | last turn completed cleanly | the lifecycle channel, or projection when a turn finished without a `Stop` hook |
| `running` | 200 | actively working a task | the lifecycle channel |
| `idle` | 100 | wired in, nothing in flight | the lifecycle channel |

`waiting` and `failed` sit close enough that the time curve can interleave an older failure above a fresh ask, and the lowest attention state still starts above the highest calm state. Among the calm statuses, a finished `success` outranks a working `running` row, because a result deserves one look while work in progress does not. Only the base weight lives here; the time curve that multiplies it is [sidebar.md](../sidebar/sidebar.md).

The phase ([`TurnPhase`](../../../crates/rimz/src/agents/lifecycle.rs)) is `reasoning`, `acting`, or `parked` inside a running turn, and `idle` everywhere else. The machine normalizes that invariant, so a resting agent mid-phase is unrepresentable.

The glyph, animation, and color for each are the canonical table in [the interface legend](../../interface/sidebar.md#reading-the-glyphs); this doc owns the transitions, not the painting.

## The rollup

[`reduce_agent_states`](../../../crates/rimz/src/store/snapshot/project.rs) folds the `agent.lifecycle` events into one `AgentState` keyed by `(kind, agent_id)`, where `agent_id` is the session id, so two concurrent agents of the same kind never share a row.

Each event is a *partial* update. How the reducer treats a field the event omits is that field's **lifetime**, and the lifetimes are the rule the types themselves do not state:

| Lifetime | Rule | Fields |
| --- | --- | --- |
| identity | set once when the session registers, stable thereafter | `agent_id`, `kind`, `parent_agent_id` |
| placement | replaced when the session moves into a resumed pane or a newer observation re-owns it | `pane`, `runtime_owner` |
| set-once | fills from the first usable observation, then stays stable | `first_prompt` |
| activity | replaced by the latest event, where *clearing* it is meaningful: an idle agent has no `task` | `status`, `task`, `last_activity` |
| carry-forward | persists until a newer value arrives; a missing value never resets it | `model`, `effort`, `context_pct`, `context_window`, `prompt`, `description`, `transcript_path`, `recent_prompts` |
| accumulated | increments from durable named events and survives replay | `tool_calls` |
| live-derived | computed at snapshot time from the live pane or git over the stored fallback | `worktree_path`, `worktree_branch` |
| transient heads | opened and closed by signals, painted over the base status | the turn [phase](#turn-phase), the [compaction bracket](#the-compaction-bracket) |

[`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) and [`AgentState`](../../../crates/rimz/src/agents/state.rs) are the field catalog. Four rules earn a note:

- A subagent's `task` is the one activity-lifetime exception: it holds the child's type (`Explore`, and so on) and carries forward as identity, so a finished child stays labeled when its `SubagentStop` omits the type.
- `first_prompt` accepts the first user prompt that is neither blank nor a harness control turn. It labels an unnamed session ahead of the changing latest `prompt`; an adapter-emitted `description`, such as a native title or child task description, supersedes it through the normal carry-forward path.
- The live-derived fields follow the pane, which knows its current directory every tick, so `worktree_path` and `worktree_branch` track a `git checkout`. Pinning them at registration would be the branch-tracking bug.
- `model` is stored canonicalized: a trailing capability tag is stripped (`claude-opus-4-8[1m]` becomes `claude-opus-4-8`). The tag rides only the fresh-launch payload, so without canonicalization the carry-forward would flip the label the first time a suffix-less event arrived. Canonicalizing at reduce time pins one stable label while the event log stays faithful to the raw payload.

### Instance identity and age

`last_activity` is always the agent's *own* latest event, never inherited from a previous instance of the same kind. Identity is required: a payload carrying no session id is quarantined (logged under `rimz::agent::lifecycle` and folded to nothing), so two distinct session-less instances can never merge into one row. No agent hits this today, since every adapter carries a session id on its first state-bearing event, but it is where a real per-instance key would land if a future agent emitted session-less transitions.

## The state machine

An adapter emits an agent-agnostic **lifecycle signal**: the *intent* a native event carries ([`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs)). Every `agent.lifecycle` event carries its signal explicitly in the params; a payload without one folds to nothing. Which native event maps to which signal is each provider's adapter doc. `ToolUsed.name` carries the provider's tool name when that adapter supports tool statistics; replay increments the session's `tool_calls` map, while unnamed legacy and unsupported-adapter events preserve lifecycle behavior without inventing a count.

One pure transition function, [`step`](../../../crates/rimz/src/agents/lifecycle.rs), folds a signal onto the prior state. It is the single home for every transition, reused identically for root agents and subagents: the reducer calls it on replay to derive the rollup, and Store calls it under the workspace lock against the latest durable state for each fresh event. Store returns the transition classification in its receipt, so hook ingestion can log anomalies without re-reading state.

### The lifecycle event envelope

[`LifecycleEvent`](../../../crates/rimz/src/agents/lifecycle/event.rs) is the versioned public projection of one durable transition: event and workspace identity, agent lineage, the complete signal, before-and-after status, phase, transition classification, and the compaction and waiting-clear facts. Store builds the envelope at the same commit seam that appends `agent.lifecycle`, so an append suppressed by lifecycle policy produces no envelope and a derived subagent append produces its own envelope in log order.

Hook ingestion dispatches these envelopes through a static reactor table. Each reactor declares a [`SignalSet`](../../../crates/rimz/src/agents/lifecycle/event.rs) beside its action; queued-delivery nudges, terminal run wakes, and ended-agent message archival consume the same vocabulary that external harnesses receive. Reactors remain latency paths and re-check durable state before acting.

[`rimz events follow`](../../reference/cli/events.md) folds the same `step` function over the durable log and emits one envelope per conforming lifecycle record. The stream starts from a read-only rollup seed at the live edge, or from an empty state at the start of the current generation under `--replay`, and drains a rotated tail before reading the new active log. The durable log remains truth; polling and reactor dispatch only surface it sooner.

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

 parked     : a clean end with background work in flight keeps the rollup running;
              display shows ✓ ⋯ bg, and a prompt wake resumes the same boundary
 subagents  : subagent started establishes the child row in running;
              subagent stopped resolves it to success / failed, and that terminal
              verdict absorbs a reordered late start
 compacting : a transient head held over any status, and the one signal that
              clears waiting outright (the bracket below)
 waiting    : awaiting input enters from any status; the next turn, tool,
              compaction open, or compaction close returns the row to running
 ended      : session end or reap stamps the durable row; runtime hides it,
              audit retains it for explicit resume
```

The edges, precisely:

| Signal | From → to | Note |
| --- | --- | --- |
| `registered` | *(none)* → `idle` | establishes the row; with `subagent_started`, the only signal that does |
| `turn_started` | any → `running` | opens the turn in the `reasoning` phase and stamps a fresh prompt boundary; a parked running row resumes and carries the prior boundary |
| `turn_ended`, clean | `running` → `success` | the turn resolved; the phase rests |
| `turn_ended`, errored | `running` → `failed` | the error bit always wins |
| `turn_ended`, clean with background work in flight | `running` → `running` | the rollup parks; display projects `success` with `⋯ bg` |
| `turn_interrupted` | any → `idle` | the provider or user canceled the turn, closing it with no result; carries the provider turn id when known |
| `awaiting_input` | any → `waiting` | a blocking prompt ([`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs): permission, plan approval, or question) holds the row for a human; a repeat restamps it |
| `subagent_started` | *(none)* → `running` | establishes the child row, keyed by the child's own id; a terminal `success` or `failed` child holds its verdict when a reordered start arrives late |
| `subagent_stopped` | `running` → `success` / `failed` | the child's terminal verdict, kept through the parent's turn |
| `tool_used` (mutating) | resting or *(none)* → `running`, reconciled; `waiting` → `running` | completed work proves a turn, except a completion carrying the id of the turn just interrupted is trailing canceled-turn output and is ignored; attention rows hold; a keyed ask clears only for a tool with the same native key, while either key being absent preserves the any-completion fallback; the first file-editing tool moves the phase to `acting` |
| `compacting` | status and phase held, except `waiting` → `running`, reconciled | stamps the [compaction head](#the-compaction-bracket); compaction proves the native prompt released the pane, so it clears a waiting row and the ask behind it |
| `compaction_ended` | auto → `running` (phase carried) · manual → prior resting status resumed, or stale `running` → `idle` · trigger unknown → held · any close on `waiting` → `running` | closes and counts an open [bracket](#the-compaction-bracket) |
| `ended` | held, row stamped ended | the reducer records `ended_at`; reaching `step` preserves the prior lifecycle state as an ignored no-op |
| `lost` | held | legacy `rimz.agent-lost` marker retained for log replay compatibility, reaching `step` as an ignored no-op |

A `turn_ended` resolves the turn to `success`, or to `failed` on its error bit, never back to `idle`; a provider-native `turn_interrupted` closes the turn at `idle` without a result. One `turn_ended` exception keeps the rollup running: a clean end also carrying `parked_on_background` means the main thread parked on still-in-flight background work, so lifecycle truth stays `running` in the `parked` phase while the sidebar immediately displays `success` and retains the `⋯ bg` marker (the provider-specific detection is in [adapter_claude.md](./adapter_claude.md#hooks-and-lifecycle)). Claude wakes a parked parent by injecting the finished background task's notification as a `UserPromptSubmit`: folded onto the parked running rollup, that `turn_started` resumes the same logical turn, restores the displayed row to `running`, and carries `turn_started_at` forward so child verdicts stay visible through the delegation wave. Once the turn reaches a clean end, the next prompt stamps fresh and clears past-turn verdicts.

A `subagent_stopped` resolves the *child* the same way, and the sidebar keeps that `✓` or `!` through the parent's turn ([sidebar.md](../sidebar/sidebar.md#sub-agent-lists)).

`waiting` arrives like every other status: a blocking hook classifies as [awaiting-user](./adapter.md#two-channels) and records an `awaiting_input` signal, and `step` moves the row to `waiting`. The reducer stamps `waiting_since` from the signal, and the shared guard [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs) reserves pane input while that durable ask postdates activity. An activity heartbeat newer than the ask releases the prompt without waiting for a durable clear, so an agent answered in its own UI returns to work at once; a *keyed* ask instead holds through newer activity until its correlated durable clear, because a parallel sibling tool also advances the heartbeat. A provider interruption marker newer than `last_activity` releases a waiting row to `idle`, proving Esc cancelled the native prompt when no lifecycle hook reports the cancellation. A transition off `waiting` sets `waiting_cleared`, and the ingestion path appends those durably even for signals it would otherwise skip, so a non-mutating approved tool still clears the row on replay.

### Fail-soft, never silent

`step` is total: an unexpected `(state, signal)` pair never panics and never freezes. It takes the signal's natural edge, because the agent is authoritative about its own activity, and tags the result [`TransitionKind::Reconciled`](../../../crates/rimz/src/agents/lifecycle.rs) with the state it overrode and why.

The reducer discards the tag. Store returns it in the lifecycle receipt, and the ingestion path logs it once per fresh event under `rimz::agent::lifecycle` to stderr: `warn!` on a reconciled edge, `debug!` on an ignored no-op, `error!` on a quarantined identity, keeping hook stdout for [the decision channel](./adapter.md#hook-stdout-is-the-decision-channel). Drift between the model and reality leaves a structured breadcrumb instead of a wrong-but-quiet row. The headline case is in the edge table: a tool observed on a resting row proves the rollup is stale, so `step` moves it to `running` and logs the edge.

Adding a new signal variant is a deliberately high bar, since every variant costs an edge in this one table; the gate is in [adapter.md](./adapter.md#extending-the-signal-vocabulary).

### Turn phase

The phase is the running turn's shape: the agent owns its status, and RimZ derives the phase from the turn's own hook events. Every turn opens in `reasoning` (`turn_started` and `subagent_started` set it), and the sidebar paints the thinking head while the turn reads, searches, and decides. The turn's first **file-editing** tool moves it to `acting` (`tool_used { edits: true }`, each adapter's file-writing subset read through `tool_edits_files`). The trigger is always a hook event, never prompt or transcript content.

```text
turn starts ──► reasoning  ──first file-editing tool──► acting ──► turn ends
                    │                                                  ▲
                    └── a research turn that never edits a file ───────┘
clean end with background work still in flight ──► parked (rollup running; display ✓ ⋯ bg)
```

- A research turn stays in the thinking head end to end: searches, reads, and shell commands write no file, so a turn that answers without editing stays in `reasoning`.
- A shell command is work without writing. It keeps the row live and leaves the phase in place, and a phase that left `reasoning` never re-arms mid-turn.
- Any turn boundary rests the phase. `turn_ended` and `subagent_stopped` drop it, and the next prompt re-arms it. A clean end with background work still in flight parks it instead.
- Subagents own separate `agent_id`s, so a child observation never mutates its parent's phase. Providers with bracket-only identity fold `subagent_started` and `subagent_stopped` and keep child per-tool work on its heartbeat; Codex hooks carry distinct child identity on prompt, tool, permission, and compaction progress, so those signals fold onto the child row with rollout enrichment.

A `parked` row displays `success` immediately and retains its phase solely to paint `⋯ bg`, while silent `reasoning` and `acting` rows escalate as stalled. The phase vocabulary is painted once, in [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

### The compaction bracket

Compaction is a transient head over the status. The opening signal (`Compacting`) stamps `compacting_since` and holds the prior status and phase, so the sidebar pulses the compaction head over whatever the agent was doing. A `waiting` row is the one exception: a compaction runs only once the native prompt has released the pane, so the open is proof the ask resolved and the row clears to `running` — the reconciled edge that recovers a prompt dismissed with `esc`, which reaches RimZ through no hook of its own. Without it a stale `?` outranks the head at the [lead cell](../../interface/sidebar.md#reading-the-glyphs), where a human-blocked glyph always wins, and the card paints an ask the agent no longer holds. The session's next lifecycle signal closes the bracket: `step` emits the close as a transition fact ([`Transition::compaction_closed`](../../../crates/rimz/src/agents/lifecycle.rs)), and the rollup increments the durable `compaction_count` from it exactly once per bracket. The card surfaces the count as `↻ N` on the context line.

`compaction_ended` is the explicit close, and its trigger decides where the agent lands:

| Trigger | Lands | Because |
| --- | --- | --- |
| known automatic | `running`, interrupted phase carried | automatic compaction happens mid-turn |
| known manual | prior resting status resumed; a stale `running` row rests to `idle` | `/compact` runs between turns |
| absent | prior status and phase held | the provider reported no trigger bit |

A close that rests the agent advances `turn_started_at`, retiring the prior turn's subagents the same as a fresh prompt or `/clear`; an automatic mid-turn close resumes the turn and holds the boundary. Redundant close signals are idempotent, since an absent bracket closes nothing, and the projection expires the head past a short display window, so a crash mid-compact can never pulse it forever.

A compaction signal for a session the rollup has never seen folds to nothing unless provider evidence names the predecessor condensed into that session. A linked Codex compact close seeds the successor immediately and carries the exact `compacted_from` identity; an unlinked rotated id still waits for its first turn or tool signal, so aborted compactions cannot create unreapable ghosts that steal pane primacy.

## Displayed status

The rollup keeps the agent-owned truth. `snapshot.agents`, as `rimz sidebar snapshot` reports it, always holds the true lifecycle status; the *displayed* status is a projection over it, and two layers compute it.

[`effective_status`](../../../crates/rimz/src/agents/state.rs) is the cheap shared projection every read path uses, so `rimz pane list` and message delivery agree with the sidebar about hookless state: a still-`running` turn with an active park marker reads as `paused`, a hookless plan proposal reads as `waiting`, a hookless completion or interruption reads as `success` or `idle`, and a clean end parked on background work reads as `success`.

The sidebar row projection ([`project_display_status`](../../../crates/rimz/src/store/snapshot/view/aggregate/status.rs)) adds liveness and budget-aware refinements on top. The order is a pinned contract, top rung wins:

| Rung | Condition | Displays |
| --- | --- | --- |
| 1 | a human-blocked `waiting` row | `waiting` |
| 2 | a budget park, or a turn-error certificate whose class parks the turn | `paused` |
| 3 | a turn-error certificate of a fatal class | `failed`, with the upstream label |
| 4 | a live subagent under an otherwise calm parent | `running` |
| 5 | a turn that completed without a `Stop` hook | `success` |
| 6 | a turn or ask interrupted without a terminal hook | `idle` |
| 7 | a clean turn parked on background work | `success`, retaining the `parked` phase |
| 8 | a consecutive identical-tool run reaches the attention threshold | `failed`, with the tool and repeat count |
| 9 | silent past the stall window | `paused` on a spent window, otherwise `failed` |
| 10 | nothing above applies | `effective_status` |

Rung by rung:

1. **Waiting outranks everything.** An open blocking prompt holds the row unless a newer turn-interruption marker proves Esc cancelled it, in which case the row settles to `idle`. A keyless `waiting` row that fails [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs), because activity postdating the ask proves it was answered in the pane, projects back to `running` in the `reasoning` phase until a durable clear lands.
2. **Paused** covers an agent whose latest turn stopped on a provider limit, a RimZ dollar cap, or a transient API error. No hook emits `paused`: RimZ derives it here, and it joins the cockpit tally just under the actionable attention states. A launch `budget` reads the session's cumulative live cost against a per-session runtime ledger; crossing it sends Esc and stamps the row with spend against cap. Provider `rate_limit` and `spend_limit` certificates are per-agent while their budget decision is account-scoped, and `overloaded` covers provider overload, serving capacity, 5xx-class failures, stalled streams, timeouts, and connection drops. Two conditions promote a park to `failed` instead: the auto-continue retry budget is exhausted, or a rate-limit marker survives past its window's reset with no spent window left to explain it ([providers.md](./providers.md#spent-windows-and-paused-rows)).
3. **Turn death.** A non-transient provider API error or unclassified turn-death marker escalates to `!` at once, and the card quotes the upstream or derived error label. For a still-`running` row the marker must postdate `last_activity`, so the explicit death certificate beats both live-child activity and the stall window; for a terminal `failed` row the marker must fall inside the row's current turn, so an old marker never explains a fresh failure. Any newer hook event self-clears it.
4. **Waiting on children.** An otherwise clean `idle`, `success`, or `running` agent with a live subagent projects to `running` and paints a quiet wave. The stall clock reads the row's displayed activity, which folds in the children's, so a child that just finished defers escalation too, and the durable resting parent status returns after the final child stops.
5. **Turn completion.** Codex's `/review` ends on a clean rollout `task_complete` with a non-empty `last_agent_message` and no `Stop`, so the completion marker postdates `last_activity` and settles the row instead of letting the stall window misread a finished review as failed ([adapter_codex.md](./adapter_codex.md#turn-completion-marker)). A newer prompt self-clears it.
6. **Turn interruption.** The derived marker source is provider-specific: Codex writes rollout `turn_aborted` for Esc and `/clear` mid-turn ([adapter_codex.md](./adapter_codex.md#turn-interruption-marker)); Claude writes a transcript `user` sentinel beginning `[Request interrupted by user` ([adapter_claude.md](./adapter_claude.md#turn-interruption-marker)). The marker postdates `last_activity` and settles the row as at rest with no result, instead of letting a false wait persist or the stall window misread it as failed.
7. **Parked settle** displays a clean turn parked on background work as `success` immediately, since the turn's verdict was earned and only the background chore is still humming. The display row retains the `parked` phase so `⋯ bg` keeps that pending work legible; a wake's `turn_started` re-runs the row.
8. **Tool-loop detection** catches a `running` agent whose completed tool calls keep refreshing the heartbeat while making no progress: the same tool name and canonicalized arguments repeated to the configured attention threshold project to `!` with a `loop: <tool> ×<count>` label. The next differing tool or other progress event clears the consecutive run and returns the row to `running` without human action.
9. **Stall** is the backstop for any other `running` agent silent past the configurable window. A kind with a spent, unreset budget window reads `paused`. Everything else escalates to the attention `!` ([Liveness and presence](#liveness-and-presence)).
10. **The bottom rung** is `effective_status`, which is where the hookless plan-approval projection lands: a `running` Codex row whose completed planning turn rests on a rollout `Plan` item reads as `waiting`. The normal `Stop` hook records the durable plan ask, so this marker is the missed-hook backstop that keeps the row and the message-delivery gate safe without inventing an ask record ([adapter_codex.md](./adapter_codex.md#plan-approval-marker)).

Each rung reads enrichment plus liveness, and each leaves the rollup holding the true lifecycle status: Claude transcript-death can leave the rollup `running` while Codex Stop-over-rollout-error records the rollup `failed`, and projection refines either display to `paused`. The [`displayed_status_precedence_ladder_holds`](../../../crates/rimz/src/store/snapshot/view/tests/status/stall.rs) test stacks the causes against each other, so a reordering fails the suite even when every single-cause test still passes.

The phase and head paints ride over this base: a `running` agent in `reasoning` renders the thinking head, and an open compaction bracket pulses over any base status. A projection to a non-running status drops the phase except for `success` with `parked`, the one settled shape that keeps pending background work visible.

## The instance lifecycle

An agent reaches the sidebar as an **agent instance**: a live local pane running a known agent command or hosting one live agent CLI under its pane root, bound one-to-one to its pane id, `pane_pid`, and process-start. A **session** binds to it, and the instance exits when its pane reverts to a shell.

The instance exists before any session id is known, and the lifecycle's one hard problem is joining the two. The join turns on two independent axes.

**Hook identity: stamped or daemon-routed.** A *standalone* agent runs in its pane, so the hook is a descendant of it and reads the pane environment and pid directly: it stamps the pane id onto the session. A *daemon-routed* agent runs through a background daemon, so the hook fires from the daemon (no pane environment, the daemon's shared pid) and the session is unstamped. Claude, Copilot, and Droid are standalone. Droid 0.170.0 is the narrow exception to direct `$PPID` ownership, because its canonical hook emitter is an internal exec worker whose observations are reassigned to the structurally verified outer TUI pid. Codex 0.137+ daemon-routes its hooks through the shared app-server, so even in-pane Codex sessions are unstamped ([adapter_codex.md](./adapter_codex.md#session-registration-and-launch-quirks)); Pi and interactive OpenCode run in-process in the pane and are standalone.

**Presence: in-pane or remote.** Orthogonal to hook identity is where the agent actually runs. An *in-pane* agent has a local pane with its own `pane_pid`, so you can jump to it: a standalone agent binds its pane by the stamped id, and a daemon-routed in-pane agent (a Codex CLI thin-wrapping the daemon) binds through the recovery ladder. Either way it renders as a normal, jump-able row. A *remote* agent runs only in the daemon, with no local pane (`claude remote-control --spawn worktree`, or a Codex thread started from the web): it carries a worktree but nothing to focus. RimZ does not render remote agents yet, a documented gap deferred to a future round ([sidebar.md](../sidebar/sidebar.md#presence-model)). The `claude remote-control` host pane itself is separate infrastructure, filtered out of the room and surfaced as the `⇅ rc` flag.

So the binding test is one question: does a live local pane bind the session? A stamped session binds by id, an unstamped session binds through the recovery ladder, and a session no pane binds is a remote agent.

**Hosted CLI identity is adapter-wide.** When a multiplexer exposes only a shared runtime basename such as `node`, the pane producer walks one bounded root-to-single-child process chain and classifies each full command line through the adapter registry. The outermost proven known CLI supplies the hosted kind and process start; an unreadable, branching, startless, depth-exhausted, or unclassified chain supplies none. This proof applies to every known adapter and does not consult `registers_lazily`, which governs only the recovery of an unstamped session.

The lifecycle then runs in three phases.

**Phase 1: pre-session presence.** A wired instance with no bound session yet renders as an idle agent row, so a just-launched agent reads as itself rather than a bare process. Claude reaches this at the login screen and in the short span before `SessionStart` stamps the pane; Codex and OpenCode reach it before their first real session exists; Kiro remains identity-less until its provider-owned local store yields a safe binding, while Antigravity binds on its first invocation hook or an exact local-session match. RimZ synthesizes an idle `○ <kind>` row until a lifecycle hook or local-session observation binds the real session ([`idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). Installed hooks activate hook capabilities, and the declared provider-store observation path activates session capabilities; an integration with neither active path stays a [process row](../sidebar/sidebar.md#process-rows).

**Phase 2: session binding.** A lifecycle hook arrives carrying a session id, and RimZ joins it to the right instance. A standalone hook stamped the pane id, so the join is exact and free. Only a definition whose spec sets `registers_lazily` enters the unstamped recovery ladder:

1. Hook ingestion writes a recovered same-directory pane stamp from the repaired live frame.
2. A `codex resume <session-id>` pane binds exactly.
3. Same-directory sessions pair newest-first to the latest viable pane process-start before the session's first event.

Residual ambiguity binds deterministically and appends a `binding.log.jsonl` breadcrumb. The ladder's guards and limits are [sidebar.md](../sidebar/sidebar.md#presence-model).

A native resume establishes identity and placement before this hook path: the exec wrapper appends `agent.attached` with the stable launch id exported to the process, its ambient pane, and its live process owner without emitting a lifecycle signal. Existing cards retain their lifecycle state; a discovered provider session gets an idle seed so it can identify itself before the provider's first hook. The later hook or local-session observation remains authoritative for lifecycle state.

Same-pane ownership is an adapter policy. `KeepPrimary` pins a pane to its earliest registered co-resident root session, which keeps Codex `/side` and `/btw` forks from repainting the primary card; later co-resident root clocks fold display-only onto the primary row. A provider-linked compact continuation inherits its predecessor's registration time, so it retains primacy after the predecessor leaves while unrelated forks remain subordinate. `FollowLatest` hands the pane to the latest registered root conversation, using latest activity and session id only as deterministic tie-breakers; Antigravity uses it for an in-place conversation-id switch, and Cursor for the hook-less `/clear` conversation switch. The policy applies only after kind, pane incarnation, process identity, directory, and root-session guards establish one live instance. Occupied-pane recovery still prefers unique focus evidence and otherwise admits only one resting same-kind owner, while running, waiting, ambiguous, already-known, wrong-directory, and wrong-incarnation candidates abstain.

Providers with their own local session stores normalize binding separately, through `LocalSessionObservation`. Adapters validate and discover these typed observations; the elected room producer batches the admitted workspaces per kind and publishes the normalized observations; every renderer binds only an exact session-matching publication against its current admitted panes. The observation's projection declares its source authority. `IdentityOnly` proves session identity and activity bounds but defers status, phase, prompt, wait, ask, compaction, context, and lifecycle clocks to an exact durable hook row, synthesizing idle only when adopting a provisional row or creating a session with no durable state. `Lifecycle` carries a provider-validated fold and overlays an exact durable row only when its provider activity is at least as current as durable `last_activity`; an accepted provider-native wait is pane-only and clears any durable routable ask. Exact resume identity binds first, and exact pane and session binding consumes both sides before that freshness decision, so a stale fold cannot rebind the pane through a later same-directory candidate. Fresh fallback additionally skips sessions the runtime projection has ended or expelled for a dead owner. For runtime-visible rows, it also skips sessions whose durable pane stamp names a pane absent from the live frame; exact resume and exact pane identity remain unguarded. A cwd-only fallback requires a positive pane-incarnation clock: the strongest of the live process start and RimZ's durable launch time. The observation must begin and remain active no earlier than that clock; an occupied registered pane is reserved, and a pane with neither clock fails closed until an exact hook, resume id, or process-start backfill arrives. Rejections append contained `local_session_bind_rejected` diagnostics; an exact old stamp contradicted by a newer durable launch appends the investigative `ghost_session_bind` regression signal. The caching and revalidation policy behind those reads lives in [`local_session_cache.rs`](../../../crates/rimz/src/agents/local_session_cache.rs); stamps are an optimization only, and unstable or wrong-kind inputs fail closed.

**Phase 3: instance exit.** The in-pane agent process is the liveness truth, surfaced through the pane: the CLI client is the pane's foreground process or the single hosted descendant under the pane root, so when it exits the pane reverts to a shell and stops reading as an agent. The instance leaves with no exit hook, in both launch modes. A `SessionEnd` hook (Claude) stamps the durable session ended and removes its context sidecar at once; runtime views hide the row while audit views retain its provider identity for explicit resume within retention. Codex has no `SessionEnd`, so the [reaper](#liveness-and-presence) stamps the same state after pane liveness proves the process gone.

Daemon-routed Codex hooks first name the shared app-server daemon, then the recovery ladder re-owns the local session to its in-pane CLI process and stores the full pane stamp (`pane_id`, tab id, directory, pane pid, and process start). An unbound daemon-owned session abstains from pid liveness and ages through the ghost TTL like a pidless row; the app-server loaded-thread reaper is a faster secondary signal, dropping a daemon-mode session absent from `thread/loaded/list` before the pane fold, while an unreachable daemon or untrusted list keeps every session.

## Liveness and presence

Presence comes from the live pane, with no exit event required: an agent renders only on the pane it stamped, and one whose pane reverts to a shell or closes is gone on the next snapshot. There is no `offline` status: a dead agent is a reverted shell row or no row, never a retracted store fact. The binding mechanics live in [sidebar.md](../sidebar/sidebar.md#presence-model); this section owns what the rollup contributes.

**Stamped-pane binding decides what renders; the captured pid feeds the reaper.** RimZ records the pid best-effort on each lifecycle event (`RIMZ_AGENT_PID=$PPID`, falling back to a process-ancestor walk, plus a platform process-start token to defeat pid reuse), and the reaper reads *pidless* as one ghost signal. Stamped-pane binding already keeps a stale agent off a stranger's pane, so the pid never gates rendering.

**Per-tool activity rides a runtime heartbeat.** The durable event log is turn-grained, so `last_activity` would otherwise advance only at turn boundaries. The hook touches a per-agent heartbeat ([`agent_activity`](../../../crates/rimz/src/agent_activity.rs)) on every progress-proving event (each completed tool call, the turn boundaries, subagent start and stop), and the snapshot folds the freshest touch into `last_activity`. A pre-tool event or a blocked wait touches nothing, and the heartbeat is keyed by the event's own session: a backgrounded subagent's progress touches the *child's* heartbeat, and a parent blocked on an ask keeps its `last_activity` frozen until it acts. The signal does three things:

- It keeps a busy agent's row animating.
- It escalates a `running` agent silent past the configurable stall window (30 minutes by default) to the `!` attention state.
- It recovers an answered keyless ask: once `last_activity` passes `waiting_since`, `is_awaiting_input` reads false, so an agent whose prompt was answered in its own UI returns to `running` without waiting for the next turn boundary.

Like every heartbeat it is latency, not truth: a missing file just leaves `last_activity` at the event-log timestamp.

**Estimated active time accumulates observable root-session work.** Hook ingestion opens or advances a working span on turn starts, tool progress, and compaction progress, and freezes it on waits, turn ends or interruptions, parked background work, session exit, and provider-error markers. Each `(kind, agent_id)` record lives under runtime `active-time/`, updates under a per-record flock, and publishes by atomic rename; sidebar and RimZ process restarts preserve the accumulator while the room runtime survives.

An open span extends from its latest progress signal by at most `[agents.attention] active_grace_secs` (180 seconds by default), so a silent process freezes instead of fabricating work. Later progress credits that capped tail and resumes from the new observation without bridging the idle gap. The projection stamps only root `AgentState` rows; nested subagents keep their existing start-to-now elapsed clock.

**Session death converges to the durable log.** After a publishing commit, the debounced write-path reaper appends an `Ended` observation and stamps `ended_at` for every root session whose death is provable from the store plus the process table. Death is provable when a recorded owner is dead, when a pidless or daemon-owned session is inactive past the ghost TTL, when an older session was replaced by a different process in the same pane or by a newer paneless remnant in the same worktree, when a newer fresh-lineage conversation supersedes it after `/clear` or `/new`, when a `FollowLatest` adapter reports a distinct newer id on the exact same pane and identified live agent process, or when a provider-linked compact continuation names the predecessor on that same pane and process incarnation. Rebirth materialization appends the same signal for every lost session an accepted recovery plan does not seed.

The supersession rules are the delicate part, because a child can report a distinct conversation id from the same process mid-turn. Same-process and fresh-lineage switches therefore keep an older running, waiting, or paused owner authoritative; a provably different replacement process still supersedes it. One stricter rule retires a raw-running or raw-waiting fresh root after an interrupted `/clear` or `/new`: a distinct newer fresh root must carry later activity on the exact same pane and identified agent-process incarnation, and the provider transcript must rest on an interruption marker newer than the predecessor's last activity. The compact-continuation rule also bypasses the running-owner guard because the successor names the exact predecessor and matches its pane and agent-process incarnation; a plain fork has no compact link and remains protected. Paused owners stay protected from the clear rules, forks and daemon-owned roots fail their proofs, known pane or process-start mismatches fail every same-instance proof, and any later predecessor hook self-clears the stale interruption marker by advancing `last_activity`. The plain, interrupted, and compact-continuation supersession rules bypass live-roster protection because the successor proves the predecessor yielded the slot; dead and stale candidates retain roster protection for crash recovery. Runtime expel and the snapshot-time view reap apply the store-only liveness and shared supersession rules as latency shims during the debounce window, and provider interruption evidence reaches correctness only through the durable end event.

An agent holding its own distinct pane is kept, and subagents leave transitively with their parent. Already-ended rows are skipped and never supersede an active row, so repeated reaps append nothing and a retained stamp cannot retire its replacement. This workspace-local convergence complements the cross-workspace `rimz gc`.

Address resolution enforces the same physical-instance boundary during convergence: when a live pane is bound to one session, a different root stamped on that pane is a shadowed audit record and contributes no recipient to role, kind, name, broadcast, pane, prefix, or exact-session addressing. Exact addresses miss shadowed roots like ended sessions, while durable history and message audit surfaces retain them.

Worktree removal appends a durable `Ended` observation for every matching non-live root session and bypasses live-roster protection, because successful removal is affirmative evidence that the session can no longer run there. A later lifecycle event for the same session id, including native resume registration, clears the end stamp as usual.

## Attribution

`rimz agents attribution` reads `RuntimeScope::Audit`, so a teammate remains eligible after its pane exits and the runtime projection hides it from live cards. Lane filtering applies to root records before the attribution fold; no multiplexer observation participates in correctness.

A logical member can span several session records. Provider compaction continuations and `/clear` conversations mint fresh session ids while the same contributor keeps its seat, so attribution folds by provider kind plus the first available stable slot: team and role, launch group and ordinal, explicit name, pane id, then session id. Pane-backed children then join the seat that launched them; an orphan whose parent has left the input keeps its own slot. Seat records alone supply identity, presence, and session count, while child records contribute effort and activity.

The figures keep their source boundaries. The audit rollup supplies identity, timestamps, tool calls, compaction counts, and the parent/type identity of provider subagents. RimZ's append-only conversation transcript supplies per-session prompt, agent-message, and ask counts; matched system nudges are sender-stamped and excluded, while sender handles on received agent messages provide best-effort sent counts. Historical system nudges written before sender stamping remain indistinguishable from user prompts. Each session's adapter parses its provider transcript once, and the shared price book supplies the four-way token split and dollars from those same entries. Companion child transcript entries retain their child id through that deduplication fold, so attribution can split child cost by durable subagent type without changing the parent total; missing, long, or whitespace-bearing type labels group as `other` rather than exposing task descriptions. Per-session active-time sidecars supply estimated active seconds under the configured silence grace. Runtime GC can remove those sidecars before the audit record or provider transcript disappears, so an unavailable active-time figure stays `null`; absent transcript pricing likewise stays `null` rather than becoming zero.

Membership drops only a seat that never opened a turn and has no active time, asks, messages, tool calls, compactions, subagents, tokens, or recorded cost. The audit rollup's durable `turn_started_at` keeps a contributor eligible after runtime GC removes its active-time sidecar, including adapters whose transcripts supply no spend or named-tool signal.

## Enrichment

The store and explicit events decide routing, ranking, and state; enrichment paints the row. `task`, `context_pct`, `context_window`, and `total_tokens` are enrichment: display-only and redactable. A missing value means "the agent did not report it", never zero. The sidebar still paints a context bar for every observed agent, drawing an unreported gauge at a visible 0% baseline.

`context_window` is the model's window in tokens, and uniformly across agents it is the model's max **input** tokens. The gauge numerator counts input-side occupancy only (`input + cache`, never output; see [`context_used_tokens`](../../../crates/rimz/src/agents/state.rs)), so a model that splits its window into separate input and output caps scales against the input cap. Each adapter resolves the window its own way: Claude from the payload model id, where `[1m]` widens it; Codex from the rollout's `model_context_window`; OpenCode from its model catalog. The card's identity line renders it (`258k`, `1M`), preferring the fresher out-of-band reading from `AgentContext` when one exists.

Where those numbers come from is the adapter's business: the three context sources and their reading rules are [adapter.md](./adapter.md#context-sources). These are bare token counts, so `payload_mode` gates the *content* of high-frequency payloads, never these gauges.

### Rich context (`AgentContext`)

Some agents publish far richer per-session data out of band than their hooks carry: context-window accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, thread preview, PR info, version, effort. The transport differs per agent and lives in its adapter doc; `observe_context` normalizes transport payloads into the agent-agnostic [`AgentContext`](../../../crates/rimz/src/agents/context.rs). Every field is `Option` and tolerantly parsed, so a sparse or evolved payload always parses and the renderer draws whatever subset is present. The account and balance subset (plan, metered, rate-limit windows) folds into the provider dashboard; its mapping and aggregation are [providers.md](./providers.md).

This is high-frequency display-only enrichment, so it does **not** ride the event log. RimZ writes a latest-wins per-session sidecar, one atomic file per `(kind, agent_id)` under the runtime `agent_context/` directory, from CLI producer paths (statusline feed, hook ingestion, detached refresh helpers, the Codex stat-gated backstop). `rimz sidebar snapshot` folds each record onto its `AgentState`.

The sidecar lives wholly off the durable path (store first; sidebar wakeups are latency, not truth) and dies with the session: a session-end event removes it, an ended row stays hidden and has no sidecar to enrich, and `rimz gc` sweeps old files. The file sits under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

A few `AgentContext` fields reach past display. Turn-error, turn-settle, and native-attention markers feed the shared status projection, which is how every read path agrees about [hookless state](#displayed-status) without inventing durable records.

## See also

- [adapter.md](./adapter.md) — where observations come from: the adapter boundary, the hook path, and context sources.
- [providers.md](./providers.md) — accounts, balances, spend, and pricing.
- [sidebar.md](../sidebar/sidebar.md) — presence binding, ranking, and how the rollup becomes a row.
- [the interface legend](../../interface/sidebar.md#reading-the-glyphs) — the glyph, animation, and color for every status and phase.
- [store.md](../store.md) — the durable event log the rollup replays.
