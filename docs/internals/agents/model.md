# The agent model

This page owns how RimZ turns an agent's native events into one durable state per session, how that state moves, and how the status a reader sees is projected from it. An adapter turns each native event into an [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) ([adapter.md](./adapter.md)); the rollup described here folds those observations into one [`AgentState`](../../../crates/rimz/src/agents/state.rs) per session; the sidebar projects that state into a row ([sidebar.md](../sidebar/sidebar.md)).

```text
native agent event
  │  the adapter normalizes it                                   adapter.md
  ▼
AgentLifecycleObservation ──► one Store lifecycle transaction    store.md
  │  replay folds each signal through step()                     this page
  ▼
AgentState, one per (kind, agent_id)
  │  live panes bind instances to sessions                       instances.md
  │  the heartbeat and context sidecars refine the status        this page
  ▼
sidebar row                                                      sidebar.md
```

Everything on this page is provider-neutral. An integration that emits well-formed observations gets the state machine, ranking, liveness, and attention routing without code of its own. Which native event carries which signal is each adapter's page ([adapter_claude.md](./adapter_claude.md), [adapter_codex.md](./adapter_codex.md), and eleven siblings). How a session binds to a pane and when RimZ declares it dead is [instances.md](./instances.md). Accounts, spend, and pricing are [providers.md](./providers.md), and the credit report built over the audit rollup is [attribution.md](./attribution.md). The commitments behind the model are in [DESIGN.md](../../../DESIGN.md).

## Terms

| Term | Meaning |
| --- | --- |
| agent kind | A wired integration (`claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `cursor`, `droid`, `kiro`, `qwen`, `grok`), described by an [`AgentSpec`](../../../crates/rimz/src/agents/definition.rs). |
| session | Identity: the id the agent's own hooks report. Every durable fact attaches to a session, keyed `(kind, agent_id)`. |
| agent instance | Presence: a live local pane running a known agent right now, read from the multiplexer on every snapshot. Joining instances to sessions is [instances.md](./instances.md). |
| rollup entry | The one `AgentState` per session that store replay derives. The sidebar enriches and renders it. |
| signal | The provider-neutral intent one native event carries ([`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs)). |

## Status and phase

A session's lifecycle state is a status, the running turn's phase, and a transient compacting head painted over both ([`LifecycleState`](../../../crates/rimz/src/agents/lifecycle.rs)). The glyph, animation, and color for each value are [the interface legend](../../interface/sidebar.md#reading-the-glyphs); this page owns what the values mean and how they move.

The statuses rank by a base weight, most attention-hungry first ([`status_weight`](../../../crates/rimz/src/store/snapshot/view/score.rs)). The rollup stores only `waiting`, `failed`, `success`, `running`, and `idle`; `paused` and `sleeping` exist only in the [displayed status](#displayed-status).

| Status | Weight | Meaning |
| --- | --- | --- |
| `waiting` | 600 | blocked on a human decision in the agent's own UI |
| `failed` | 560 | the last turn errored, stalled, or looped |
| `paused` | 400 | stopped on a provider limit, a dollar cap, or a transient API error (display only) |
| `success` | 300 | the last turn completed cleanly |
| `running` | 200 | working a turn |
| `sleeping` | 150 | at rest with a pending one-shot wait armed (display only) |
| `idle` | 100 | wired in, nothing in flight |

`waiting` and `failed` sit close enough that the time curve can lift an older failure above a fresh ask, and the lowest attention weight still starts above the highest calm one. A finished `success` outranks a working `running` row because a result deserves one look and work in progress does not. The time curve that multiplies the base weight is [sidebar.md](../sidebar/sidebar.md#attention-ranking-and-the-cap).

The phase ([`TurnPhase`](../../../crates/rimz/src/agents/lifecycle.rs)) is `reasoning`, `acting`, or `parked` while the status is `running`, and `idle` for every other status. `step` enforces that rule on every transition, so a resting row with a live phase cannot exist in the rollup. [Turn phase](#turn-phase) covers how it moves.

## The rollup

The reducer folds `agent.lifecycle` events, in log order, into one `AgentState` keyed by `(kind, agent_id)`. Because `agent_id` is the session id, two concurrent agents of the same kind never share a row. Production replay runs through `reduce_agent_states_seeded_with_identity` in [`project.rs`](../../../crates/rimz/src/store/snapshot/project.rs), which also assigns card names and applies [launch identity inheritance](./instances.md#launch-identity-across-conversations) after each event.

Each event is a partial update. `carried_base` clones the prior row, `assemble_agent_state` overlays what the event carries, and the rule for a field the event omits is that field's lifetime. The types do not state these rules, so the table does. [`AgentState`](../../../crates/rimz/src/agents/state.rs) is the full field catalog.

| Lifetime | Fields | Rule |
| --- | --- | --- |
| identity | `kind`, `agent_id`, `registered_at` | Set when the row is created. A compaction continuation takes its predecessor's earlier `registered_at` so it keeps the predecessor's ownership rank. |
| lineage | `parent_agent_id` | Set by the event that creates the row. On an existing parentless row only a `SubagentAdopted` event can set it; an established parent never changes. |
| launch | `launch_id`, `profile`, `login`, `mode`, `isolation`, `role`, `team`, `channel`, `launch_group`, `launch_ordinal`, `launch_depth` | Set from launch parameters, then carried. A same-instance successor conversation inherits them ([instances.md](./instances.md#launch-identity-across-conversations)). |
| registration | `account_key`, `transcript_path` | Replaced whenever an event carries a value, otherwise carried. The event projection strips both from root events that do not establish identity (`transcript_path` also survives on `turn_ended`), so in practice a registration sets them. |
| placement | `pane`, `runtime_owner` | Replaced by the event's pane stamp and owner. A bare stamp naming the same pane keeps the richer prior stamp, and a daemon owner never displaces the session's own agent-process owner. |
| checkout | `worktree_path`, `worktree_branch`, `worktree_branches` | The path is taken from an identity-establishing event and otherwise pinned to the prior value. A branch is accepted only when the event's path is absent or matches the pinned path; `worktree_branch` is the latest accepted branch and `worktree_branches` accumulates every one. |
| set-once | `first_prompt` | The first prompt that is neither blank nor a harness control turn, then stable. |
| activity | `status`, `phase`, `last_activity`, `ended_at`, and a root's `task` | Replaced by every event. An event that omits `task` clears it, because an idle agent has no task. Every event except `ended` clears `ended_at`. |
| carry-forward | `model`, `effort`, `usage` (`context_pct`, `context_window`, `total_tokens`), `prompt`, `description`, `recent_prompts`, `origin`, `compacted_from`, `budget` | Replaced when an event carries a value; a missing value never resets it. `recent_prompts` keeps the newest 16. |
| counters | `tool_calls`, `compaction_count` | Incremented from durable events, so replay reproduces them. |
| turn boundaries | `turn_started_at`, `user_turn_started_at` | Advanced by the signals in [the edge table](#edges); otherwise carried. |
| open ask | `waiting_since`, `open_ask`, `interrupted_turn_id` | `waiting_since` and `open_ask` live only while the row is `waiting`. `interrupted_turn_id` is recorded by `turn_interrupted` and cleared by `registered` or a newly opened turn. |
| compaction | `compacting_since`, `compacted_awaiting_prompt` | `compacting_since` marks an open [compaction bracket](#the-compaction-bracket). `compacted_awaiting_prompt` is set by a sent compact command or a successful manual close, cleared only by `turn_started`, and consulted only for adapters with a native turn-start hook. |

Five of these rules need their reason stated:

- A provider-native subagent's `task` holds the child's type (`Explore`, and so on) and carries forward, so a finished child stays labeled when its `SubagentStop` omits the type. A root's `task` follows the activity rule.
- `first_prompt` labels an unnamed session ahead of the changing latest `prompt`. An adapter-emitted `description`, such as a native title or a child task description, supersedes it for display.
- `account_key` is the opaque fingerprint of the provider account a root session registered on; it is never the credential. A re-registration of the same session id (a Claude `--resume`) rebinds the row to the login in force at that moment. It lives in the rollup, and never in [rich context](#rich-context), because the provider dashboard partitions live budget readings by it ([providers.md](./providers.md#producer-aggregation)).
- `model` is stored canonicalized: a trailing capability tag is stripped (`claude-opus-4-8[1m]` becomes `claude-opus-4-8`). The tag rides only a fresh-launch payload, so without canonicalization the carried label would flip the first time a tagless event arrived. The event log keeps the raw payload.
- The live row reads `worktree_path` and falls back to the pane's current directory only when no path is stored. The sidebar looks up the displayed branch from that path, so a `git checkout` inside the tree shows on the card while the rollup's branch record stays tied to observed events.

Identity is required. An event with no session id is quarantined: ingestion logs it at `warn!` under `rimz::agent::lifecycle`, and the reducer folds it to nothing, so two session-less instances can never merge into one row. Every shipped adapter carries a session id on its first state-bearing event. For a session the rollup has not seen, the reducer also quarantines a `lost` marker, a typeless `subagent_stopped`, and a compaction signal with no provider-named predecessor ([the compaction bracket](#the-compaction-bracket)). `registered` and `subagent_started` are the signals meant to create a row; any other signal that creates one is logged at `debug!` under `rimz::agent::binding`.

## The state machine

One pure, total function, [`step`](../../../crates/rimz/src/agents/lifecycle.rs), folds a signal onto the prior `LifecycleState` and is the only home for transitions. Roots and subagents share it. The reducer calls it on replay, and Store calls it under the workspace lock against the latest durable state for each fresh event, returning the transition in its receipt so hook ingestion can log anomalies without re-reading state. Every `agent.lifecycle` event carries its signal explicitly; a payload without one folds to nothing.

Store does not append every observation. An unnamed `tool_used` that neither mutates nor edits is proof of work only, and the writer appends it only when it is child-owned or its transition closes a compaction bracket, clears a wait, or reconciles a stale status ([`store/writer/lifecycle.rs`](../../../crates/rimz/src/store/writer/lifecycle.rs)). A named `tool_used` is always appended, and replay counts it into `tool_calls`.

```text
 ●
 │ registered
 ▼
idle ──── turn started ───► running ──── turn ended ──┬── clean ───► success
 ▲                           │  ▲                     └── errored ─► failed
 │                           │  │
 └──── turn interrupted ─────┘  └── turn started, or a tool on idle / success

awaiting input        any status ──► waiting; the next tool, turn, or compaction returns it to running
clean end, bg work    running stays running in the parked phase; display shows success with ⋯ bg
ended                 running or waiting ──► failed (a reaped end rests at idle); resting statuses hold
```

### Edges

| Signal | Status | Also |
| --- | --- | --- |
| `registered` | any → `idle` | Establishes a row. On a row that has opened a turn, advances both turn boundaries, which retires the prior turn's subagents (a `/clear`). |
| `turn_started` | any → `running` | Opens the `reasoning` phase and stamps `turn_started_at`. `user_turn_started_at` advances only for a user-authored prompt, not a harness-delivered one. On a `parked` row it resumes the same turn and keeps both boundaries. |
| `turn_ended` clean | → `success` | Rests the phase. |
| `turn_ended` errored | → `failed` | The error bit wins over the parked bit. |
| `turn_ended` clean, `parked_on_background` | → `running` | Moves the phase to `parked` ([turn endings](#turn-endings-and-parked-turns)). |
| `turn_interrupted` | any → `idle` | The provider or user canceled the turn, which closes it with no result. Records the provider turn id when known. |
| `awaiting_input` | any → `waiting` | Rests the phase, stamps `waiting_since`, and opens the ask ([`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs): permission, plan approval, or question). A repeat restamps it. |
| `subagent_started` | → `running` | Establishes the child row under the child's own id and opens `reasoning`. A child already at `success` or `failed` holds its verdict, so a late reordered start is ignored. |
| `subagent_stopped` | any → `success` or `failed` | The child's verdict, by its error bit. |
| `tool_used` | `idle`, `success`, or no row → `running` (reconciled); `running` and `waiting` → `running`; `failed` holds | Ignored when it carries the id of the turn just interrupted (trailing output). On a `waiting` row with a keyed ask, a tool carrying a different native key is a parallel sibling and is ignored. Moves the phase ([turn phase](#turn-phase)). |
| `compacting` | held; `waiting` → `running` (reconciled) | Opens the [compaction bracket](#the-compaction-bracket). |
| `compaction_ended` | by trigger ([the bracket](#the-compaction-bracket)) | Closes the bracket. |
| `ended` | `running` or `waiting` → `failed`; other statuses hold | Rests the phase and stamps `ended_at`. A [reaped end](#observed-ends-and-reaped-ends) rests an active row at `idle` instead. |
| `lost` | held | A legacy `rimz.agent-lost` marker kept parseable for log replay; `step` ignores it. |

An `ended` mid-turn delivered nothing, so it takes the failed disposition of [`terminal_disposition`](../../../crates/rimz/src/agents/lifecycle.rs), which supervised runs share. A run record that already holds a more specific terminal outcome, such as `timed_out` or `canceled`, keeps it ([scripting.md](../harness/scripting.md)). An open compaction bracket closes with the row. Runtime views hide an ended row, and audit views keep it for explicit resume.

### Turn endings and parked turns

A `turn_ended` resolves the turn to `success`, or to `failed` on its error bit, never back to `idle`; only `turn_interrupted` closes a turn at `idle`. The exception is a clean end carrying `parked_on_background`: the main thread stopped while background work is still in flight. The rollup stays `running` in the `parked` phase, and the displayed status reads `success` at once while the card keeps a `⋯ bg` marker for the pending work. Claude is the provider that reports this ([adapter_claude.md](./adapter_claude.md#hooks-and-lifecycle)).

Claude wakes a parked parent by injecting the finished background task's notification as a `UserPromptSubmit`. That `turn_started`, folded onto the parked row, resumes the same logical turn: the displayed status returns to `running`, and both turn boundaries carry forward so child verdicts stay visible through the delegation wave. After a clean end, the next prompt stamps a fresh provider turn, and only a user-authored one advances `user_turn_started_at` and retires past-turn child verdicts ([sidebar.md](../sidebar/sidebar.md#sub-agent-lists)).

A parked row is still `running`, so an `ended` fails it like any other running row.

### Subagents

A `subagent_stopped` resolves the child row, and the sidebar keeps that `✓` or `!` through the parent's user-authored turn ([sidebar.md](../sidebar/sidebar.md#sub-agent-lists)). A child owns its own `agent_id`, so its signals never move the parent's status or phase.

A pane-backed launched child that never reaches `subagent_stopped` (it timed out, was stopped, or its provider died mid-turn) resolves through `ended` instead: `failed` for an observed end, `idle` for a reaped one. Either way the retained child row is at rest, which keeps it out of the parent's live-child count ([subagents.md](../harness/subagents.md#the-lifecycle-end-to-end)).

Providers whose hooks identify a child only at its brackets fold `subagent_started` and `subagent_stopped` and keep the child's per-tool work on its heartbeat. Codex hooks carry the child's identity on prompt, tool, permission, and compaction events as well, and Claude stamps the child's `agent_id` on every payload fired inside it, so those signals fold onto the child row ([adapter_codex.md](./adapter_codex.md), [adapter_claude.md](./adapter_claude.md#subagents)).

### Waiting and asks

A blocking hook classifies as [awaiting-user](./adapter.md#two-channels) and records `awaiting_input`, and `step` moves the row to `waiting`. From there, the shared guard [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs) decides whether the ask still holds, and message delivery reserves pane input while it does. It holds in three cases:

- The row is `waiting` and no activity is newer than `waiting_since`. An activity heartbeat newer than the ask releases it at once, so an agent answered in its own UI returns to work without waiting for a durable clear.
- The row is `waiting` on a keyed ask (one carrying a native key). A keyed ask holds through newer activity until a tool with the same native key completes, because a parallel sibling tool also advances the heartbeat.
- A `running` row's context sidecar carries a provider native-wait or plan-proposal marker newer than `last_activity`. This covers native dialogs without inventing a durable ask.

A transition off `waiting` sets `waiting_cleared`, and Store appends it even for a proof-of-work tool it would otherwise skip, so a non-mutating approved tool still clears the row on replay. A compaction open or close also clears a waiting row, because a compaction runs only after the native prompt releases the pane ([the compaction bracket](#the-compaction-bracket)). A provider interruption marker newer than `last_activity` displays a waiting row as `idle`, which is how RimZ learns that Esc cancelled a native prompt when no hook reports it.

### Fail-soft, never silent

`step` is total: no `(state, signal)` pair panics or freezes the row. An unexpected pair takes the signal's natural edge, because the agent is authoritative about its own activity, and the result is tagged [`TransitionKind::Reconciled`](../../../crates/rimz/src/agents/lifecycle.rs) with the status it overrode and why. The common case is a tool observed on a resting row, which proves the rollup is stale.

The reducer discards the tag. Hook ingestion logs it once per fresh event under `rimz::agent::lifecycle` to stderr: `warn!` for a reconciled edge and `debug!` for an ignored no-op. A subagent event with unusable identity (a missing child or parent id, or a child id equal to its parent) is quarantined at `error!` and emits no observation. Hook stdout stays reserved for [the decision channel](./adapter.md#hook-stdout-is-the-decision-channel). Drift between the model and reality leaves a structured breadcrumb instead of a quietly wrong row.

Adding a signal variant costs an edge in this one table, so the bar is deliberately high; the gate is in [adapter.md](./adapter.md#extending-the-signal-vocabulary).

### Turn phase

The phase is the running turn's shape, derived only from hook signals, never from prompt or transcript content. `turn_started` and `subagent_started` open `reasoning`, where the sidebar paints the thinking head. The first file-editing tool moves the turn to `acting` (`tool_used` with `edits`, each adapter's file-writing subset read through `tool_edits_files`).

```text
turn starts ──► reasoning ──first file-editing tool──► acting ──► turn ends
                    │                                                 ▲
                    └────── a turn that never edits a file ───────────┘
clean end with background work in flight ──► parked
```

A non-editing tool, such as a search or a shell command, keeps `reasoning` when the turn is already in it, so a research turn stays in the thinking head from start to end. Once a turn leaves `reasoning` it never re-arms mid-turn. A tool that arrives outside `reasoning` moves the phase to `acting` whether or not it edits: that covers a resting row reconciled back to `running`, a waiting row whose ask was answered, and a parked row that visibly went back to work.

Every turn boundary rests the phase: `turn_ended`, `turn_interrupted`, `subagent_stopped`, `awaiting_input`, `registered`, and `ended`. A clean end with background work parks it instead, and compaction holds it.

### The compaction bracket

Compaction is a transient head over the status. `compacting` stamps `compacting_since` and holds the prior status and phase, so the sidebar pulses the compaction head over whatever the agent was doing. The one status it changes is `waiting`, which moves to `running`: a compaction runs only after the native prompt releases the pane, so the open proves the ask is gone, including a prompt dismissed with Esc that no hook reports. Without this edge a stale `?` would win the [lead cell](../../interface/sidebar.md#reading-the-glyphs) and the card would paint an ask the agent no longer holds.

The session's next signal of any kind closes the bracket. `step` reports the close as [`Transition::compaction_closed`](../../../crates/rimz/src/agents/lifecycle.rs), and the rollup increments `compaction_count` once per bracket that did not close as failed. The card shows the count as `↻ N` on the context line.

`compaction_ended` is the explicit close, and its trigger decides where the agent lands:

| Close | Lands | Because |
| --- | --- | --- |
| automatic | `idle`, `success`, or `waiting` → `running` with the phase carried; `running` stays; `failed` holds | automatic compaction happens mid-turn |
| manual | `running` → `idle`; `waiting` → `running`; other statuses hold | `/compact` runs between turns, so a still-running row is stale |
| trigger unknown, or failed | status and phase held; `waiting` → `running` | the provider reported no trigger, or the compaction did not complete |

A successful close that leaves the row at rest after a turn has opened advances both turn boundaries, retiring the prior turn's subagents the same way `/clear` does. An automatic close that resumes a turn from rest opens a fresh turn boundary. A close with no open bracket and nothing to change is ignored. The projection expires the head 90 seconds after `compacting_since` (`COMPACTING_WINDOW_SECS`), so a crash mid-compaction cannot pulse forever.

A compaction signal for a session the rollup has never seen folds to nothing unless the observation names the predecessor it condensed (`compacted_from`). A linked Codex compact close seeds the successor immediately and carries the predecessor's identity; an unlinked rotated id waits for its first turn or tool signal. That rule keeps aborted compactions from creating ghost rows that steal pane ownership.

### Observed ends and reaped ends

An `ended` is either observed or inferred. The supervised wrapper's `rimz.agent-ended`, a provider `SessionEnd` hook, and room rebirth's end for a session it did not recover ([`harness/rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs)) report an end that happened. The [reaper](./instances.md#session-death) infers one from the store and the process table and names its reason as the event name: `ReapedSuperseded`, `ReapedDead`, or `ReapedStale`; worktree retirement uses `WorktreeRemoved` ([`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)).

An inference is not a verdict on the turn. For exactly those four event names, [`assemble_agent_state`](../../../crates/rimz/src/store/snapshot/project.rs) overrides `step` and lands a prior `running` or `waiting` row at `idle` instead of `failed`. `step` itself is unchanged: every other end, including one with an absent or unrecognized event name, takes the failed edge, and a `failed` recorded before the reap stays `failed`.

Resting at `idle` lets a live session that raced the reap recover: its next tool or turn signal clears `ended_at` and moves the row back to `running` through the ordinary edge. A session that really ended stays at rest, with its elapsed time frozen at the end stamp, and never counts as a live child holding its parent in `running`. The rule is the same for roots and launched children, so a reaped child rests under its parent without showing `!`; a supervised child's run record still carries the specific outcome the digest reports.

## Publishing transitions

[`LifecycleEvent`](../../../crates/rimz/src/agents/lifecycle/event.rs) is the versioned public projection of one durable transition: event and workspace identity, agent lineage, the complete signal, status and phase before and after, the transition classification, and the compaction and waiting-clear facts. Store builds it at the same commit that appends `agent.lifecycle`, so an append that lifecycle policy suppresses produces no envelope, and a derived subagent append produces its own envelope in log order.

Hook ingestion dispatches envelopes through a static reactor table. Each reactor declares a [`SignalSet`](../../../crates/rimz/src/agents/lifecycle/event.rs) beside its action; queued-message nudges, terminal run waits, and ended-agent message archival are reactors. They are latency paths and re-check durable state before acting.

[`rimz events follow`](../../reference/cli/events.md) reads the same vocabulary. The read-only [`store::follow`](../../../crates/rimz/src/store/follow.rs) follower folds `step` over the durable log and emits one envelope per conforming lifecycle record. It starts from a rollup seed at the live edge, or from empty state at the start of the current log generation under `--replay`, and drains a rotated tail before reading the new active log.

## Displayed status

The rollup holds the agent-reported lifecycle status, and `rimz sidebar snapshot` reports it unchanged in `snapshot.agents`. The displayed status is a projection over it, computed in two layers.

[`effective_status`](../../../crates/rimz/src/agents/state.rs) is the cheap projection every read path shares, so `rimz pane list`, message delivery gates, and the sidebar agree about state no hook reported. It reads the rollup, the budget park, pending waits, and the context sidecar's markers, in this order:

1. A `running` row with a native-wait or plan-proposal marker reads `waiting`.
2. A row with a budget park, unless it is `waiting`, reads `paused`.
3. A `waiting` row with an interruption marker reads `idle`.
4. A `running` row with a turn-error marker of a pausing class reads `paused`.
5. A `running` row with a completion marker reads `success`; with an interruption marker, `idle`; in the `parked` phase, `success`.
6. An `idle` or `success` result with a pending wait reads `sleeping`.

A marker counts only when it is newer than `last_activity`, so any newer hook event clears it.

### The sidebar ladder

The sidebar row projection, [`project_display_status`](../../../crates/rimz/src/store/snapshot/view/aggregate/status.rs), adds liveness, children, and provider budget windows on top. The first rung whose condition holds decides the displayed status:

| Rung | Condition | Displays |
| --- | --- | --- |
| 1 | a `waiting` row that `is_awaiting_input` holds | `waiting` |
| 2 | a `running` row with a native-wait or plan-proposal marker | `waiting` |
| 3 | a budget park | `paused`, with spend against cap |
| 4 | a turn-error marker of a pausing class | `paused`; `failed` with the upstream label once retries are exhausted or the limit window reset with no spent window |
| 5 | a turn-error marker of a fatal class | `failed`, with the upstream label |
| 6 | a live subagent under an `idle`, `success`, or `running` parent | `running` |
| 7 | a completion marker (a turn finished without a `Stop` hook) | `success` |
| 8 | an interruption marker (a turn or ask cancelled without a terminal hook) | `idle` |
| 9 | a clean turn parked on background work | `success`, keeping the `parked` phase |
| 10 | identical consecutive tool calls reach the attention threshold | `failed`, labelled `loop: <tool> ×<count>` |
| 11 | a `running` row silent past the stall window | `paused` when the kind has a spent window, otherwise `failed` |
| 12 | none of the above | `effective_status` |

A `waiting` row that fails rung 1 first resolves: with an interruption marker it becomes `idle` and continues down the ladder skipping rung 3; without one, activity newer than the ask proves it was answered in the pane, so it continues as `running` in the `reasoning` phase until a durable clear lands. The [`displayed_status_precedence_ladder_holds`](../../../crates/rimz/src/store/snapshot/view/tests/status/stall.rs) test stacks the causes against each other, so a reordering fails the suite even when every single-cause test passes.

The rungs that need more than their row:

- **Native waits and plans (2).** A provider marker says a native dialog is open without a durable ask. The Codex case is a `running` row whose completed planning turn rests on a rollout `Plan` item: the normal `Stop` hook records a durable plan ask, and this marker is the backstop when that hook was missed ([adapter_codex.md](./adapter_codex.md#plan-approval-marker)).
- **Paused (3, 4).** No hook emits `paused`. A launch `budget` reads the session's cumulative live cost against a per-session runtime ledger; crossing it sends Esc and stamps the park ([budget.md](../harness/budget.md)). The pausing turn-error classes are `rate_limit` and `spend_limit`, which are per-agent markers with an account-scoped budget decision, and `overloaded`, which covers provider overload, serving capacity, 5xx-class failures, stalled streams, timeouts, and connection drops. The promotion to `failed` happens when auto-continue has exhausted its retries, or when a rate-limit marker outlives its window's reset with no spent window left to explain it ([providers.md](./providers.md#spent-windows-and-paused-rows)).
- **Turn death (5).** A non-transient provider API error or unclassified turn-death marker escalates to `!` at once, and the card quotes the upstream or derived label. For a `running` row the marker must postdate `last_activity`, so an explicit death certificate beats both live-child activity and the stall window. For a `failed` row the marker must fall inside the current turn, so an old marker never explains a fresh failure.
- **Waiting on children (6).** The parent paints a quiet wave. The stall clock reads the row's displayed activity, which folds in the children's, so a child that just finished defers escalation too. The durable resting status returns after the last child stops.
- **Completion (7).** Codex `/review` ends on a clean rollout `task_complete` with a non-empty `last_agent_message` and no `Stop`; the marker settles the row instead of letting the stall window misread a finished review ([adapter_codex.md](./adapter_codex.md#turn-completion-marker)).
- **Interruption (8).** Codex writes rollout `turn_aborted` for Esc and for `/clear` mid-turn ([adapter_codex.md](./adapter_codex.md#turn-interruption-marker)); Claude writes a transcript `user` entry beginning `[Request interrupted by user` ([adapter_claude.md](./adapter_claude.md#turn-interruption-marker)).
- **Tool loops (10).** A looping agent completes tools and refreshes its heartbeat, so it can never stall. A run of the same tool name with the same canonicalized arguments reaching `[agents.attention] tool_repeat_attention_after` (20 by default) is the progress-failure certificate. The next differing tool or other progress clears the run without human action.
- **Stall (11).** The backstop for any other silent `running` row, after `[agents.attention] stalled_after_secs` (30 minutes by default). The next heartbeat clears it.

Each rung leaves the rollup's lifecycle status untouched. Claude transcript-death can leave the rollup `running` while Codex's `Stop` over a rollout error records it `failed`, and the projection displays either as `paused` when the error class pauses.

A projection to any status other than `running` drops the phase, except `success` or `sleeping` over `parked`, the settled shapes that keep pending background work visible. The compaction head pulses over any displayed status.

### Sleeping

After the ladder settles, an `idle` or `success` result with `pending_waits` displays `sleeping`; `running`, `waiting`, `failed`, and `paused` never do, and a parent with a live child stays `running`. A parked clean turn can therefore display `sleeping` while keeping its `parked` phase. Enrichment reads pending waits from instance-sourced, one-shot delivery rows in the loop catalog that match the workspace root and the target kind and session, without writing a store event. Timers, watched commands, and one-shot or deadline signal deliveries count; standing subscriptions and recurring clocks do not, even when a recurring row has a deadline.

A pending wait needs a parsed trigger. A self-wait timer's due time anchors at `wait_meta.armed_at`. A human one-shot `loop add --wait --at HH:MM` row has no arm timestamp, so its due time is the next occurrence after the snapshot clock in the configured timezone.

`sleeping` is neither attention nor actionable, so `needs_a_look` is false: the unread episode and any configured success notification open when the wait cycle finishes at `success`, not on the intermediate rest, and an existing unread episode persists across sleep. The cockpit tally orders `waiting → failed → paused → success → running → sleeping → idle`. The `Done` and `Any` delivery gates open for a sleeping agent, so a message can start a turn without consuming the armed wait; `Resume` does not open. Reply waits treat it as completed, and idle compaction stays eligible under its usual guards. `--when` still reads the raw lifecycle status and does not accept `sleeping`, and `agent.idle` remains a turn-boundary event.

## Activity clocks

`last_activity` is the session's own latest event, never inherited from another instance. The durable log is turn-grained, so on its own `last_activity` would advance only at turn boundaries. Hook ingestion therefore touches a per-session runtime heartbeat ([`agent_activity`](../../../crates/rimz/src/agent_activity.rs)) on every progress-proving event (each completed tool call, the turn boundaries, subagent start and stop), and the snapshot folds the freshest touch into `last_activity`. A pre-tool event or a blocked wait touches nothing. The heartbeat is keyed by the event's own session, so a background subagent's progress touches the child's heartbeat, and a parent blocked on an ask keeps its clock frozen until it acts.

The heartbeat keeps a busy row animating, drives the [stall rung](#the-sidebar-ladder), and releases a keyless ask answered in the pane once `last_activity` passes `waiting_since`. It is latency, not truth: a missing heartbeat file leaves `last_activity` at the event-log timestamp.

Estimated active time accumulates a root session's observable work. Hook ingestion opens or advances a working span on turn starts, tool progress, and compaction progress, and freezes it on waits, turn ends and interruptions, parked background work, session exit, and provider-error markers. An open span extends past its latest progress signal by at most `[agents.attention] active_grace_secs` (180 seconds by default), so a silent process stops accruing instead of fabricating work; later progress credits that capped tail and resumes without bridging the gap. Each `(kind, agent_id)` record lives under the runtime `active-time/` directory, updates under a per-record flock, and publishes by atomic rename, so the accumulator survives sidebar and RimZ restarts while the room runtime lives. Only root rows carry it; a nested subagent shows its start-to-now elapsed clock.

## Enrichment

The store and explicit events decide routing, ranking, and state; enrichment paints the row. `task`, `usage.context_pct`, `usage.context_window`, and `usage.total_tokens` are display-only and redactable. A missing value means the agent did not report it, never zero, although the sidebar still draws an unreported context gauge at a visible 0% baseline.

`context_window` is the model's maximum input tokens for every agent. The gauge's numerator counts input-side occupancy only, input plus cache and never output ([`context_used_tokens`](../../../crates/rimz/src/agents/state.rs)), so a model with separate input and output caps scales against the input cap. Each adapter resolves the window its own way (Claude from the payload model id, where `[1m]` widens it; Codex from the rollout's `model_context_window`; OpenCode from its model catalog), and the card's identity line renders it (`258k`, `1M`), preferring a fresher reading from rich context when one exists. The context sources and their reading rules are [adapter.md](./adapter.md#context-sources). These are bare token counts, so `payload_mode`, which gates the content of high-frequency payloads, never hides them.

### Rich context

Some agents publish far more per-session data out of band than their hooks carry: context-window accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, thread preview, PR info, version, and effort. `observe_context` normalizes each transport's payload into the provider-neutral [`AgentContext`](../../../crates/rimz/src/agents/context.rs). Every field is an `Option` parsed tolerantly, so a sparse or newer payload still parses and the renderer draws whatever is present. The account and balance subset (plan, metered state, rate-limit windows) feeds the provider dashboard ([providers.md](./providers.md)). `AgentContext` carries no account fingerprint, so a sidecar rewritten on every render can never change which account a session's windows count against; that stays with the rollup's `account_key`.

Rich context is high-frequency and display-only, so it never rides the event log. CLI producer paths (the statusline feed, hook ingestion, detached refresh helpers, and the Codex stat-gated backstop) write a latest-wins sidecar, one atomic file per `(kind, agent_id)` under the runtime `agent_context/` directory, and `rimz sidebar snapshot` folds each record onto its `AgentState`. The directory sits under the per-user runtime root (mode `0700`), the same exposure as the heartbeat and diff-stats caches. A session-end event removes the sidecar, and `rimz gc` sweeps old files.

A few `AgentContext` fields reach past display: the turn-error, turn-settle, and native-attention markers feed [`effective_status`](#displayed-status) and the sidebar ladder, which is how every read path agrees about state no hook reported without inventing durable records.

## See also

- [adapter.md](./adapter.md): where observations come from, the hook path, and context sources.
- [instances.md](./instances.md): binding sessions to panes, pane ownership, and session death.
- [attribution.md](./attribution.md): durable effort credit folded from the audit rollup.
- [providers.md](./providers.md): accounts, balances, spend, and pricing.
- [sidebar.md](../sidebar/sidebar.md): presence binding, ranking, and how the rollup becomes a row.
- [store.md](../store.md): the durable event log the rollup replays.
