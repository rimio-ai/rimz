# The agent model

This page owns how RimZ turns an agent's native events into one durable state per session, how that state moves, and how the status a reader sees is projected from it. An adapter turns each native event into an [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) ([adapter.md](./adapter.md)). The rollup described here folds those observations into one [`AgentState`](../../../crates/rimz/src/agents/state.rs) per session, and the sidebar projects that state into a row ([sidebar.md](../sidebar/sidebar.md)).

```text
native agent event
  │  the adapter normalizes it                                   adapter.md
  ▼
AgentLifecycleObservation ──► one Store lifecycle transaction    store.md
  │  replay folds observations through AgentState::transition    this page
  ▼
AgentState, one per (kind, agent_id)
  │  live panes bind instances to sessions                       instances.md
  │  the heartbeat and context sidecars refine the status        this page
  ▼
sidebar row                                                      sidebar.md
```

Everything on this page is provider-neutral. An integration that emits well-formed observations gets the state machine, ranking, liveness, and attention routing without code of its own. Which native event carries which signal is each adapter's page ([adapter_claude.md](./adapter_claude.md), [adapter_codex.md](./adapter_codex.md), and sixteen siblings). How a session binds to a pane and when RimZ declares it dead is [instances.md](./instances.md). Accounts and balances are [providers.md](./providers.md), spend and pricing [spending.md](./spending.md), and the credit report built over the audit rollup is [attribution.md](./attribution.md). The commitments behind the model are in [DESIGN.md](../../../DESIGN.md).

## Terms

| Term | Meaning |
| --- | --- |
| agent kind | A wired integration (`claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `cursor`, `droid`, `kiro`, `qwen`, `grok`, `hermes`, `qoder`, `omp`, `muse`, `minimax`), described by an [`AgentSpec`](../../../crates/rimz/src/agents/definition.rs). |
| session | Identity: the id the agent's own hooks report. Every durable fact attaches to a session, keyed `(kind, agent_id)`. |
| agent instance | Presence: a live local pane running a known agent right now, read from the multiplexer on every snapshot. Joining instances to sessions is [instances.md](./instances.md). |
| rollup entry | The one `AgentState` per session that store replay derives. The sidebar enriches and renders it. |
| signal | The provider-neutral intent one native event carries ([`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs)). |

## Status and phase

A session's lifecycle state is a status, the running turn's phase, and a transient compacting head painted over both ([`LifecycleState`](../../../crates/rimz/src/agents/lifecycle.rs)). The glyph, animation, and color for each value are [the interface legend](../../interface/sidebar.md#reading-the-glyphs). This page owns what the values mean and how they move.

The statuses rank by a base weight, most attention-hungry first ([`status_weight`](../../../crates/rimz/src/store/snapshot/view/score.rs)). The rollup stores only `waiting`, `failed`, `success`, `running`, and `idle`. `paused` and `sleeping` exist only in the [displayed status](#displayed-status).

| Status | Weight | Meaning |
| --- | --- | --- |
| `waiting` | 600 | blocked on a human decision in the agent's own UI |
| `failed` | 560 | the last turn errored, stalled, or looped |
| `paused` | 400 | stopped on a provider limit, a dollar cap, or a transient API error (display only) |
| `success` | 300 | the last turn completed cleanly |
| `running` | 200 | working a turn, an in-flight wait, or a background shell |
| `sleeping` | 150 | at rest on a timer, signal, condition, or file watch, including through a child (display only) |
| `idle` | 100 | wired in, nothing in flight |

`waiting` and `failed` sit close enough that the time curve can lift an older failure above a fresh ask, and the lowest attention weight still starts above the highest calm one. A finished `success` outranks a working `running` row because a result deserves one look and work in progress does not. The time curve that multiplies the base weight is [sidebar.md](../sidebar/sidebar.md#attention-ranking-and-the-cap).

The phase ([`TurnPhase`](../../../crates/rimz/src/agents/lifecycle.rs)) is `reasoning`, `acting`, or `parked` while the status is `running`, and `idle` for every other status. `AgentState::transition` enforces that rule for snapshots and real edges, so a resting row with a live phase cannot exist in the rollup. [Turn phase](#turn-phase) covers how it moves.

## The rollup

The reducer folds `agent.lifecycle` events, in log order, into one `AgentState` keyed by `(kind, agent_id)`. Because `agent_id` is the session id, two concurrent agents of the same kind never share a row. Production replay runs through `reduce_agent_states_seeded_with_identity` in [`store/snapshot/project.rs`](../../../crates/rimz/src/store/snapshot/project.rs), which also assigns card names and applies [launch identity inheritance](./instances.md#launch-identity-across-conversations) after each event.

Each event is a partial update. `carried_base` clones the prior row, `assemble_agent_state` overlays what the event carries, and the rule for a field the event omits is that field's lifetime. The types do not state these rules, so the table does. [`AgentState`](../../../crates/rimz/src/agents/state.rs) is the full field catalog. An `AgentState` field bumps both `SNAPSHOT_VERSION` ([`store/snapshot/view.rs`](../../../crates/rimz/src/store/snapshot/view.rs)) and `ROLLUP_CACHE_VERSION` ([`store/snapshot/fold.rs`](../../../crates/rimz/src/store/snapshot/fold.rs)).

| Lifetime | Fields | Rule |
| --- | --- | --- |
| identity | `kind`, `agent_id`, `registered_at` | Set when the row is created. A compaction continuation takes its predecessor's earlier `registered_at`, so it keeps the predecessor's ownership rank. |
| lineage | `parent_agent_id` | Set by the event that creates the row. On an existing parentless row only a `SubagentAdopted` event can set it. An established parent never changes. |
| prompt baseline | `system_prompt_digest` | Optional lowercase SHA-256 of a fresh launch's configured system-prompt inputs. A launch event's present value replaces it, and an absent value keeps it. Lifecycle and attach events preserve it, and resume and fork establish no new baseline. |
| launch | `launch_id`, `prior_launch_ids`, `launch_warnings`, `profile`, `profile_scope`, `login`, `record`, `tier`, `mode`, `isolation`, `role`, `team`, `workflow`, `channel`, `checkout`, `launch_group`, `launch_ordinal`, `launch_depth`, `launched_by` | Set from launch parameters, then carried. The fields with their own rule follow the table. |
| registration | `account_key`, `transcript_path` | Replaced whenever an event carries a value, otherwise carried. The event projection strips both from root events that do not establish identity (`transcript_path` also survives on `turn_ended`), so in practice a registration sets them. |
| placement | `pane`, `runtime_owner` | Replaced by the event's pane stamp and owner. A bare stamp naming the same pane keeps the richer prior stamp, and a daemon owner never displaces the session's own agent-process owner. |
| on-screen conversation | `on_screen_at` | An observation with `on_screen: true` replaces it with that event's ingress timestamp. With absent, null, or false, a placement change clears it: [`same_agent_instance`](../../../crates/rimz/src/store/session_death.rs) compares the prior and projected pane and runtime owner. Same-instance events and reaper ends keep it. It survives rotation and cache reload and is never inherited through launch or compaction identity. Old records read it as `None`. Only ownership reads it; it stays off the card projection. |
| process location | `worktree_path`, `worktree_branch`, `worktree_branches` | An identity-establishing event, or one with no event name, sets the path when it carries one. Any other event keeps the prior path and fills it only when none is stored. A branch is accepted only when the event's path is absent or equals the resulting path. `worktree_branch` is the latest accepted branch, and `worktree_branches` accumulates every one. |
| set-once | `first_prompt` | The first prompt that is neither blank nor a harness control turn, then stable until shortened to an ended-row label. `AgentState::observe_prompt` owns the set-once rule. |
| activity | `status`, `phase`, `last_activity`, `ended_at`, and a root's `task` | Replaced by every event. An event that omits `task` clears it, because an idle agent has no task. Every event except `ended` clears `ended_at`. |
| carry-forward | `model`, `effort`, `usage` (`context_pct`, `context_window`, `total_tokens`), `prompt`, `description`, `recent_prompts`, `origin`, `compacted_from`, `budget` | Replaced when an event carries a value. A missing value never resets it. Live rows keep the newest 16 `recent_prompts`, each cut to its first 512 characters. Ending a row clears that history and shortens `prompt` and `first_prompt` to single-line labels of at most 160 characters, with trailing whitespace dropped after the cut. |
| background shells | `background_shells` | Carried forward, then extended, replaced, or trimmed by the event's `BackgroundShellReport`. A shell already listed keeps its first `started_at`. `ended` and a real `registered` edge clear the list before the report applies; a state snapshot preserves it. See [parked turns](#turn-endings-and-parked-turns). |
| retired at a compaction | `usage`'s current-window figures (`context_pct`, `total_tokens`, the input split, `output_tokens`), `retired_context_tokens`, `compaction_retired` | A completed compaction drops the carried figures and records their occupancy ([the compaction bracket](#the-compaction-bracket)). `usage.context_window` and `usage.run_total_tokens` stay carry-forward. |
| counters | `tool_calls`, `compaction_count` | Incremented from durable events, so replay reproduces them. |
| turn boundaries | `turn_started_at`, `user_turn_started_at`, `keepalive_since`, `turn_ended_at`, `ping_turn`, `pinged_at` | Advanced by the signals in [the edge table](#edges), otherwise carried. Registration carries `turn_ended_at` unchanged. A ping-only turn moves only `ping_turn`, `pinged_at`, and `keepalive_since` ([keepalive pings](#keepalive-pings)). `pinged_at` survives registration, and a rebind to a different session clears it. |
| open ask | `waiting_since`, `open_ask`, `interrupted_turn_id` | `waiting_since` and `open_ask` live only while the row is `waiting`. `interrupted_turn_id` is recorded by `turn_interrupted` and cleared by `registered` or a newly opened turn. |
| queued asks | `queued_asks` | Edited by the event's `ask_queue` and cleared when the row lands outside `running` and `waiting` ([waiting and asks](#waiting-and-asks)). |
| turn identity | `started_turn_id`, `superseded_turn_id` | A `turn_started` whose id differs from `started_turn_id` replaces it (an id-less start clears it) and moves the replaced id, when there is one, into `superseded_turn_id`. A repeated start for the same id, and every other signal, carry both. |
| compaction | `compacting_since`, `compacted_awaiting_prompt` | `compacting_since` marks an open [compaction bracket](#the-compaction-bracket). `compacted_awaiting_prompt` is set by a sent compact command or a successful close the bracket section names, and cleared only by a real `turn_started`. |

Five launch fields carry a rule of their own. `workflow` keeps the attempt reference across native attach and turn events. `checkout` keeps the lane path when an empty-dir launch runs the process somewhere else. A launch id that a later event replaces moves into `prior_launch_ids`, which is carried and inherited like `launch_id`. `record` keeps the launch's verbatim model, effort, and agent base whatever model the provider reports later, and a record-carrying attach replaces `record` and `tier` and overlays a present `mode`. Each `agent.launch_warnings` event replaces `launch_warnings`, with an empty list after a clean launch, and a same-instance successor conversation inherits them ([instances.md](./instances.md#launch-identity-across-conversations)).

These lifetimes describe the fold's state. [State snapshots](#state-snapshots) preserve edge-owned evidence, apart from clearing a stale wait and stamping an unknown turn start. The [ended-row text rule](../store.md#the-read-path) applies in every fold, including reads of legacy carryover: it retains bounded `first_prompt` and `prompt` labels and drops recent-prompt history and pane-command text. Explicit reads and revived rows keep those labels; rotation writes them into durable carryover.

Five of these rules need their reason stated:

- A provider-native subagent's `task` holds the child's type (`Explore`, and so on) and carries forward, so a finished child stays labeled when its `SubagentStop` omits the type. A root's `task` follows the activity rule.
- `first_prompt` labels an unnamed session ahead of the changing latest `prompt`. An adapter-emitted `description`, such as a native title or a child task description, supersedes it for display.
- `account_key` is the opaque fingerprint of the provider account a root session registered on, never the credential. A re-registration of the same session id (a Claude `--resume`) rebinds the row to the login in force at that moment. It lives in the rollup, and never in [rich context](#rich-context), because the provider dashboard partitions live budget readings by it ([providers.md](./providers.md#producer-aggregation)).
- `model` is stored canonicalized: a trailing capability tag is stripped (`claude-opus-4-8[1m]` becomes `claude-opus-4-8`). The tag rides only a fresh-launch payload, so without canonicalization the carried label would flip the first time a tagless event arrived. The event log keeps the raw payload. Root lifecycle hooks seed a missing `model` and `effort` from `RIMZ_AGENT_MODEL` and `RIMZ_AGENT_EFFORT`, else from the adapter's `configured_identity`, only while the rollup holds no value for that field. A provider-observed value is never replaced by that seed.
- The live row reads `worktree_path` and falls back to the pane's current directory only when no path is stored. Sidebar grouping and Git facts use the lane path (`checkout` before `worktree_path`), so a `git checkout` inside that tree shows on the card while the rollup's branch record stays tied to observed events.

Identity is required. An event with no session id is quarantined: ingestion logs it at `warn!` under `rimz::agent::lifecycle`, and the reducer folds it to nothing, so two session-less instances can never merge into one row. For a session the rollup has not seen, the reducer also quarantines a `lost` marker, a typeless `subagent_stopped`, and a compaction signal with no provider-named predecessor ([the compaction bracket](#the-compaction-bracket)). `registered` and `subagent_started` are the signals meant to create a row. Any other signal that creates one is logged at `debug!` under `rimz::agent::binding`.

### Keepalive pings

A ping-only turn is not agent activity. It is a `TurnStarted` whose prompt is made only of `CACHE_KEEPALIVE` sections, folded onto a row that rests by its durable lifecycle (`AgentState::rests_in_lifecycle`: raw `idle` or `success`, or `running` in the `parked` phase) with no open compaction bracket, together with its close. [`AgentState::transition`](../../../crates/rimz/src/agents/state.rs), the wrapper the writer and the fold both call around `step`, returns both edges as `Ignored` with `Transition.ping` set. Status, phase, `last_activity`, every turn stamp, the turn ids, and derived signals stay as they were.

Three fields move. `ping_turn` is set while the ping is open. The open sets `keepalive_since` to the prior `keepalive_since`, else the prior `last_request_at()`, so a run of pings keeps the last real request it followed. The ping's own close stamps `pinged_at`, which [`last_request_at`](#activity-clocks) reads.

While a ping is open, a turn report carrying the id of a real turn the row knows (the started turn the ping followed, or a superseded one) is ignored too and leaves the ping open. Any other signal during a ping clears `ping_turn` and folds as the work the ping became.

## The state machine

The shared [`AgentState::transition`](../../../crates/rimz/src/agents/state.rs) decision folds observations onto the prior `LifecycleState`: real edges use the pure, total [`step`](../../../crates/rimz/src/agents/lifecycle.rs), with separate snapshot and keepalive rules. Roots and subagents share it. The reducer calls it on replay, and Store calls it under the workspace lock against the latest durable state for each fresh event. Store persists that transition for drainer recovery and returns it in its receipt, so hook ingestion can log anomalies without re-reading state. Every `agent.lifecycle` event carries its signal explicitly, and a payload without one folds to nothing.

Store does not append every observation. An unnamed `tool_used` that neither mutates nor edits is proof of work only. The writer appends it only when it is child-owned or its transition closes a compaction bracket, clears a wait, or reconciles a stale status ([`store/writer/lifecycle.rs`](../../../crates/rimz/src/store/writer/lifecycle.rs)). A named `tool_used` is always appended, and replay counts it into `tool_calls`.

```text
 ●
 │ registered
 ▼
idle ──── turn started ───► running ──── turn ended ──┬── clean ───► success
 ▲                           │  ▲                     └── errored ─► failed
 │                           │  │
 └──── turn interrupted ─────┘  └── turn started, or a tool on idle / success

awaiting input        any status ──► waiting; the next tool, turn, compaction, or ask resolved returns it to running
clean end, bg work    running stays running in the parked phase; display shows success, or running while shells run
ended                 running or waiting ──► failed (a reaped end rests at idle); resting statuses hold
```

### State snapshots

An observation with `state_snapshot: true` reports current state, not an edge. OpenCode's `client_idle` carries `registered`, and `client_running` carries `turn_started`, both without a submitted prompt. No new lifecycle signal variant is needed.

| Prior status | Idle snapshot | Running snapshot |
| --- | --- | --- |
| first sight | `idle` | `running`, with the unknown turn start stamped at arrival |
| `idle`, `success`, `failed` | unchanged | `running`, with the turn start stamped at arrival |
| `running` | `idle`, without a turn end | unchanged, every clock kept |
| `waiting` | `idle`, wait cleared | `running`, wait cleared, same turn |

Snapshots update activity, usage, model and title, stamp on-screen ownership when claimed, and clear `ended_at`. They preserve registration, turn ids, user-turn clocks, keepalive, counters, background shells and compaction state. They carry no pending ask. Hook coordination records and merges their observed context, then returns before every edge consumer, on first apply and drainer replay: no `agent.started`, `agent.waiting` or `agent.ended` signal, team reaction, role registration, sent-message acknowledgement, idle-stop withdrawal, active-time or tool-clock update, transcript entry, run settlement, reactor or local cost refresh. Rotation still runs when due. The snapshot branch does not run the workspace-wide ended-session delivery sweep; the next ordinary lifecycle hook does.

### Edges

This table applies to real edges, not the state snapshots above.

| Signal | Status | Also |
| --- | --- | --- |
| `registered` | any → `idle` | Establishes a row. On a row that has opened a turn, advances both turn-start boundaries, which retires the prior turn's subagents (a `/clear`). Carries `turn_ended_at`. |
| `turn_started` | any → `running` | Records the provider turn id when known. Opens the `reasoning` phase and stamps `turn_started_at`. `user_turn_started_at` advances only for a user-authored prompt, never a harness-delivered one. Any opened turn that is not a [keepalive ping](#keepalive-pings), and a context reset, clears `keepalive_since`. On a `parked` row it resumes the same turn and keeps both boundaries. |
| `turn_ended` clean | → `success` | Rests the phase and stamps `turn_ended_at`. A [late report](#late-turn-reports) is ignored and stamps nothing. |
| `turn_ended` errored | → `failed` | The error bit wins over the parked bit. Stamps `turn_ended_at` unless ignored. |
| `turn_ended` clean, `parked_on_background` | → `running` | Moves the phase to `parked` ([turn endings](#turn-endings-and-parked-turns)) and stamps `turn_ended_at` unless ignored. |
| `turn_interrupted` | any → `idle` | The provider or user canceled the turn, which closes it with no result and stamps `turn_ended_at`. Records the provider turn id when known. A [late report](#late-turn-reports) is ignored and stamps nothing. |
| `awaiting_input` | any → `waiting` | Rests the phase, stamps `waiting_since`, and opens the ask ([`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs): permission, plan approval, or question). A repeat restamps it. |
| `ask_resolved` | `waiting` → `running`; every other status holds (ignored) | Closes the wait whose native key it carries. A different key leaves the open ask waiting. It is no proof of activity, so a close that arrives after its turn settled leaves the row settled. Declared under `awaiting_input` coverage. |
| `subagent_started` | → `running` | Establishes the child row under the child's own id and opens `reasoning`. A child already at `success` or `failed` keeps its verdict, so a late reordered start is ignored. |
| `subagent_stopped` | any → `success` or `failed` | The child's verdict, by its error bit. |
| `tool_used` | `idle`, `success`, or no row → `running` (reconciled); `running` and `waiting` → `running`; `failed` holds | Ignored when it carries the id of the turn just interrupted (trailing output). On a `waiting` row with a keyed ask, a tool carrying a different native key is a parallel sibling and is ignored. Moves the phase ([turn phase](#turn-phase)). |
| `compacting` | held; `waiting` → `running` (reconciled) | Opens the [compaction bracket](#the-compaction-bracket). |
| `compaction_ended` | by trigger ([the bracket](#the-compaction-bracket)) | Closes the bracket. A successful, non-ignored close stamps `turn_ended_at`, and a failed close carries it. |
| `ended` | `running` or `waiting` → `failed`; other statuses hold | Rests the phase and stamps `ended_at`. A [reaped end](#observed-ends-and-reaped-ends) rests an active row at `idle` instead. |
| `lost` | held | A legacy `rimz.agent-lost` marker kept parseable for log replay. `step` ignores it. |

An `ended` mid-turn delivered nothing, so it takes the failed disposition of [`terminal_disposition`](../../../crates/rimz/src/agents/lifecycle.rs), which supervised runs share. A run record that already holds a more specific terminal outcome, such as `timed_out` or `canceled`, keeps it ([scripting.md](../harness/scripting.md)). An open compaction bracket closes with the row. Runtime views hide an ended row, and audit views keep it for explicit resume.

### Late turn reports

An adapter whose provider names each turn attaches that id to `turn_started`, `turn_ended`, and `turn_interrupted`. A turn report whose id matches the row's `superseded_turn_id` (the turn the latest start replaced) arrived late, and `step` ignores it, so the turn in progress keeps its status. A report with an id the row never recorded applies: its `turn_started` was lost, and dropping the report would leave that turn `running` until the next prompt. A `turn_interrupted` for the started turn after that turn already resolved to `success` or `failed` is ignored too, because a cancel cannot undo a delivered verdict. A later `turn_ended` for the same turn still applies. When the report or the row has no id, the edges above apply unchanged. Grok is the adapter that sends ids ([adapter_grok.md](./adapter_grok.md#hooks-and-lifecycle)).

### Turn endings and parked turns

A `turn_ended` resolves the turn to `success`, or to `failed` on its error bit, never back to `idle`. Only `turn_interrupted` closes a turn at `idle`. The exception is a clean end carrying `parked_on_background`: the main thread stopped while background work is still in flight. The rollup stays `running` in the `parked` phase. The sidebar ladder displays such a row as `success`, and the [display-rest lift](#sleeping) then raises it to `running` while a background shell or a Working wait remains, or to `sleeping` for a Sleeping wait. The card keeps a `⋯ bg` marker for the pending work. Claude is the provider that reports this ([adapter_claude.md](./adapter_claude.md#turn-endings)).

Claude wakes a parked parent by injecting the finished background task's notification as a `UserPromptSubmit`. That `turn_started`, folded onto the parked row, resumes the same logical turn: the displayed status returns to `running`, and both turn boundaries carry forward so child verdicts stay visible through the delegation wave. After a clean end, the next prompt stamps a fresh provider turn, and only a user-authored one advances `user_turn_started_at` and retires past-turn child verdicts ([sidebar.md](../sidebar/sidebar.md#sub-agent-lists)).

A parked row is still `running`, so an `ended` fails it like any other running row.

The park bit says only that something is pending. Which background shells run is a separate durable list, `background_shells`, folded from the adapter's `BackgroundShellReport` under its [lifetime row](#the-rollup): a launch adds one, a task list replaces them, a finish notice drops some. The list never moves status or phase, and the sidebar lists it in the card's waits section ([adapter_claude.md](./adapter_claude.md#background-shells)).

A clean turn end does not complete a supervised run while its session is owed a harness wake: an armed one-shot wait, a wake message in flight, or live or unreported launched work. The harness keeps the run `Running` with `parked_at` until the first in-turn signal proves work resumed. That hold leaves the provider's turn verdict and `terminal_disposition` alone ([parked runs](../harness/scripting.md#parked-runs)).

### Subagents

A child owns its own `agent_id`, so its signals never move the parent's status or phase, with one exception. A `subagent_stopped` resolves the child row, and the sidebar keeps that `✓` or `!` through the parent's user-authored turn ([sidebar.md](../sidebar/sidebar.md#sub-agent-lists)).

The exception is a native prompt that some providers raise on the parent session for a child's call. When a child's `tool_used` carries the native key of the parent's open ask, Store appends a derived `SubagentAskAnswered` `tool_used` with that key to the parent, which clears the wait like the parent's own answer edge. A key identifies the call as precisely as the provider's wire allows: a tool call id where the provider sends one, and for Copilot a digest of tool name and command text, so a child running a byte-identical command also clears a parent's own ask for it. Any other child tool leaves the parent ask open, and a keyless ask never clears from a child.

A pane-backed launched child that never reaches `subagent_stopped` (it timed out, was stopped, or its provider died mid-turn) resolves through `ended` instead: `failed` for an observed end, `idle` for a reaped one. Either way the retained child row is at rest, which keeps it out of the parent's live-child count ([subagents.md](../harness/subagents.md#the-lifecycle-end-to-end)).

Providers whose hooks identify a child only at its brackets fold `subagent_started` and `subagent_stopped` and keep the child's per-tool work on its heartbeat. Codex hooks carry the child's identity on prompt, tool, permission, and compaction events as well, and Claude stamps the child's `agent_id` on every payload fired inside it, so those signals fold onto the child row ([adapter_codex.md](./adapter_codex.md), [adapter_claude.md](./adapter_claude.md#subagents)).

### Waiting and asks

A blocking hook classifies as [awaiting-user](./adapter.md#two-channels) and records `awaiting_input`, and `step` moves the row to `waiting`. From there, the shared guard [`is_awaiting_input`](../../../crates/rimz/src/agents/state.rs) decides whether the ask still holds, and message delivery reserves pane input while it does. It holds in three cases:

- The row is `waiting` and no activity is newer than `waiting_since`. An activity heartbeat newer than the ask releases it at once, so an agent answered in its own UI returns to work without waiting for a durable clear.
- The row is `waiting` on a keyed ask (one carrying a native key). A keyed ask holds through newer activity until a tool with the same native key completes, because a parallel sibling tool also advances the heartbeat.
- A `running` row's context sidecar carries a provider native-wait or plan-proposal marker newer than `last_activity`. This covers native dialogs without inventing a durable ask.

The heartbeat those rules read comes from hook events only ([activity clocks](#activity-clocks)). Statusline pushes never advance it. A snapshot read through `snapshot_cached` attaches the context sidecars, so every consumer of the guard sees the provider's markers.

A transition off `waiting` sets `waiting_cleared`, and Store appends that event even for a proof-of-work tool it would otherwise skip, so a non-mutating approved tool still clears the row on replay. A compaction open or close also clears a waiting row, because a compaction runs only after the native prompt releases the pane ([the compaction bracket](#the-compaction-bracket)).

A keyed ask also clears when its own call resolved with no hook to report it. When a keyed tool event reaches a row waiting on a different key, the hook path asks the adapter whether the provider's transcript already holds the open call's result. On a yes it records a derived answer edge for the open key ahead of the incoming event. This is the ingress-side twin of the `SubagentAskAnswered` derivation in [subagents](#subagents), and an adapter that cannot read its transcript abstains ([adapter_claude.md](./adapter_claude.md#asks-and-answers)).

A provider interruption marker newer than `last_activity` displays a waiting row as `idle` and releases its ask, keyed or not. This is how RimZ learns that Esc cancelled a native prompt when no hook reports it. The row leaves `rimz asks` and stops reserving pane input, while the durable question record still closes on the next prompt.

Non-blocking questions live beside the blocking `open_ask`, in the ordered `queued_asks` list ([`QueuedAsk`](../../../crates/rimz/src/agents/state.rs)). An observation's optional `ask_queue` edit ([`AskQueueEdit`](../../../crates/rimz/src/agents/observation.rs)) maintains it, and the fold applies the edit in [`fold_lifecycle`](../../../crates/rimz/src/store/snapshot/project.rs) in four steps:

1. When `clear_all` is set (it defaults to false), the list empties.
2. Entries whose native key an answer in the edit names are removed.
3. Each new question with an ask id is appended, unless an entry with its native key is already listed.
4. When the resulting status is neither `running` nor `waiting`, the list empties.

The queue never moves status. [`AgentState::actionable_asks`](../../../crates/rimz/src/agents/state.rs) yields the blocking ask first, when `is_awaiting_input` holds, then the queued questions, each tagged with its `AskDelivery` (`blocking` or `async`). `rimz asks`, `rimz answer`, reply waits, and the ask materializer in [`agents/open_ask.rs`](../../../crates/rimz/src/agents/open_ask.rs) read through it. The materializer joins structured question details from the transcript by exact ask id. Status, message parking, active time, keepalive, and idle compaction read the blocking guard alone. So do the agent report rows (`crates/rimz/src/cli/agents_cmd/report.rs::build_entry`) and the ask view of `rimz agents show` (`crates/rimz/src/cli/transcript/mod.rs::latest_ask_view`), which show only the blocking ask. The sidebar copies the queue onto its card without changing the displayed status.

### Fail-soft, never silent

`step` is total: no `(state, signal)` pair panics or freezes the row. An unexpected pair takes the signal's natural edge, because the agent is authoritative about its own activity, and the result is tagged [`TransitionKind::Reconciled`](../../../crates/rimz/src/agents/lifecycle.rs) with the status it overrode and why. The common case is a tool observed on a resting row, which proves the rollup is stale.

The reducer discards the tag. Hook ingestion logs it once per fresh event under `rimz::agent::lifecycle` to stderr: `warn!` for a reconciled edge and `debug!` for an ignored no-op. A subagent event with unusable identity (a missing child or parent id, or a child id equal to its parent) is quarantined at `error!` and emits no observation. Hook stdout stays reserved for [the decision channel](./adapter.md#hook-stdout-is-the-decision-channel). Drift between the model and reality leaves a structured breadcrumb instead of a quietly wrong row.

Adding a signal variant costs an edge in this one table, so the bar is deliberately high. The gate is in [adapter.md](./adapter.md#extending-the-signal-vocabulary).

### Turn phase

The phase is the running turn's shape, derived only from hook signals, never from prompt or transcript content. `turn_started` and `subagent_started` open `reasoning`, where the sidebar paints the thinking head. The first file-editing tool moves the turn to `acting`: a `tool_used` with `edits` set, from each adapter's file-writing subset read through `tool_edits_files`.

```text
turn starts ──► reasoning ──first file-editing tool──► acting ──► turn ends
                    │                                                 ▲
                    └────── a turn that never edits a file ───────────┘
clean end with background work in flight ──► parked
```

A non-editing tool, such as a search or a shell command, keeps `reasoning` when the turn is already in it, so a research turn stays in the thinking head from start to end. Once a turn leaves `reasoning` it never re-arms mid-turn. A tool that arrives outside `reasoning` moves the phase to `acting` whether or not it edits. That covers a resting row reconciled back to `running`, a waiting row whose ask was answered, and a parked row that visibly went back to work.

Every turn boundary rests the phase: `turn_ended`, `turn_interrupted`, `subagent_stopped`, `awaiting_input`, `registered`, and `ended`. A clean end with background work parks it instead, and compaction holds it.

### The compaction bracket

Compaction is a transient head over the status. `compacting` stamps `compacting_since` and holds the prior status and phase, so the sidebar pulses the compaction head over whatever the agent was doing. The one status it changes is `waiting`, which moves to `running`. A compaction runs only after the native prompt releases the pane, so the open proves the ask is gone, including a prompt dismissed with Esc that no hook reports. Without this edge a stale `?` would win the [lead cell](../../interface/sidebar.md#reading-the-glyphs) and the card would paint an ask the agent no longer holds.

The session's next real lifecycle signal closes the bracket, with two exceptions. Another `compacting` keeps it open, and so does a signal `step` rejects before it folds anything: a `lost` marker, a [late turn report](#late-turn-reports), or an `ask_resolved` that does not close the wait. `step` reports the close as [`Transition::compaction_closed`](../../../crates/rimz/src/agents/lifecycle.rs), and the rollup increments `compaction_count` once per bracket that did not close as failed. The card shows the count as `↻ N` on its stats line, which the selected or held card carries, and every engaged card under `expanded` density. The projection expires the head 90 seconds after `compacting_since` (`COMPACTING_WINDOW_SECS`), so a crash mid-compaction cannot pulse forever.

`compaction_ended` is the explicit close, and its trigger decides where the agent lands:

| Close | Lands | Because |
| --- | --- | --- |
| automatic | `running` stays; `waiting` → `running`; resting statuses hold | only an interrupted turn resumes |
| manual | `running` → `idle`; `waiting` → `running`; other statuses hold | `/compact` runs between turns, so a still-running row is stale |
| trigger unknown, or failed | status and phase held; `waiting` → `running` | the provider reported no trigger, or the compaction did not complete |

A `compaction_ended` with no open bracket that changes neither status nor phase is ignored: it stamps no `turn_ended_at`. A successful close that lands at rest after a turn has opened advances both turn-start boundaries, ignored or not, retiring the prior turn's subagents the same way `/clear` does. It never opens a new turn. Because the start can then pass the carried `turn_ended_at`, the [request estimate](#activity-clocks) can move to that carried end.

`compacted_awaiting_prompt` suppresses keep-warm pings (Sleeping pings included) and idle compaction until a real `turn_started` clears it. A provider without a native turn-start hook ignores it, since nothing would clear it. These closes set it, in [`fold_lifecycle`](../../../crates/rimz/src/store/snapshot/project.rs):

- every successful manual close, even an ignored one
- a successful automatic close that lands at rest
- a successful triggerless close that ends an open bracket and lands at rest

A triggerless close outside a bracket leaves the marker as it was. It echoes a compaction already folded, such as Claude's second close or the `SessionStart` that Codex queues to the next turn, which after a mid-turn compaction lands on a resting row. A sent compact command also sets the marker ([`message/compact.rs`](../../../crates/rimz/src/message/compact.rs)).

A bracket that did not close as failed retires the usage measured before it. So does a successful `compaction_ended` that arrives with no bracket open, which is a compaction whose opening signal was lost. That case retires without counting, because `compaction_count` stays on the bracket. A failed compaction retires nothing.

Retirement happens in `assemble_agent_state`. It drops the carried current-window figures before it merges the closing event's usage, and records the occupancy they held (the input split when known, else `total_tokens`) as `retired_context_tokens`. A provider with no fresh figure re-reads the same transcript record at and after the close. So an event whose usage has the retired occupancy contributes none of its current-window figures, on the closing event and on every later one. Any other reading merges as usual, and the retired value stands until the next completed compaction retires a known reading.

One provider can report one compaction's end more than once (Claude sends `PostCompact` and a compact `SessionStart`). The later end finds the bracket already closed and the row carrying the post-compaction figure. `compaction_retired` marks a row whose usage a compaction has retired with only compaction ends or state snapshots folded since. A successful end on such a row retires nothing, and any other real lifecycle signal clears the mark.

The context sidecar gets the same treatment where it is joined on: `AgentState::attach_context` clears the current-window fields of a sidecar whose `used_tokens()` equals the retired value, and leaves the file as written. The card therefore shows the provider's post-compaction figure where one exists and an empty meter otherwise. `occupied_context_tokens()` is unknown to the compaction policies until a fresh reading arrives. The cost of the rule is that a genuine later reading landing on exactly the retired count reads as unknown until the next call.

A compaction signal for a session the rollup has never seen folds to nothing unless the observation names the predecessor it condensed (`compacted_from`). A linked Codex compact close seeds the successor immediately and carries the predecessor's identity. An unlinked rotated id waits for its first turn or tool signal. That rule keeps aborted compactions from creating ghost rows that steal pane ownership.

### Observed ends and reaped ends

An `ended` is either observed or inferred. Four producers report an end that happened:

- the supervised wrapper's `rimz.agent-ended`, suppressed on abrupt exits for agents parked for recovery ([fleet.md](../harness/fleet.md#reclaiming-a-pane))
- a successful subagent stop's `rimz.subagent-stopped` ([scripting.md](../harness/scripting.md#reclaiming-the-run-pane))
- a provider `SessionEnd` hook
- room rebirth's end for a session it did not recover ([`harness/rebirth.rs`](../../../crates/rimz/src/harness/rebirth.rs))

The [reaper](./instances.md#session-death) infers an end from the store and the process table and names its reason as the event name: `ReapedSuperseded`, `ReapedDead`, or `ReapedStale`. Worktree retirement uses `WorktreeRemoved` ([`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)).

An inference is not a verdict on the turn. For exactly those four event names, [`assemble_agent_state`](../../../crates/rimz/src/store/snapshot/project.rs) overrides `step` and lands a prior `running` or `waiting` row at `idle` instead of `failed`. `step` itself is unchanged: every other end, including one with an absent or unrecognized event name, takes the failed edge, and a `failed` recorded before the reap stays `failed`.

Resting at `idle` lets a live session that raced the reap recover: its next tool or turn signal clears `ended_at` and moves the row back to `running` through the ordinary edge. A session that really ended stays at rest, with its elapsed time frozen at the end stamp, and never counts as a live child holding its parent in `running`. The rule is the same for roots and launched children, so a reaped child rests under its parent without showing `!`. A supervised child's run record still carries the specific outcome the digest reports.

The twin `rimz.agent-resumed` reports an observed start for every resume, root or subagent. Before it spawns or execs the resumed provider, the exec wrapper records a bare `Registered` observation. That clears `ended_at` and restores `idle` while preserving the attached pane, owner, and launch identity. Its timestamp also sets `resumed_at`, and any other lifecycle fold clears it. Message delivery reads that stamp twice. A provider that registers at start gets no queued delivery until its registration or one delivery window ([the ordered check](../harness/messaging.md#the-ordered-check)), while a lazily registering provider gets its queued records at once. For every kind, an unconfirmed prompt written at or after the stamp stays `Sent` within a bounded number of delivery windows ([confirmation and retry](../harness/messaging.md#confirmation-and-retry)).

A provider can fork a resumed session to a new id. When it forks a root's session, it registers the new id after the resume stamp, from the revived card's pane and process, and that registration ends the revived card (`ReapedSuperseded`, appended with it). A fork that registers before the wrapper attaches the spawned provider's process to the card ends the card at that attach instead. Either way the old identity ends as the new one starts. A subagent whose provider forks on resume shows two rows while it runs: the revived id its parent messages, and the new id the provider registers. The wrapper's `rimz.agent-ended` closes a spawned provider's revived row when it exits, and an in-place exec's row ends through the reaper once its process is gone. Codex and Claude resume in place, so neither produces the double row.

## Publishing transitions

[`LifecycleEvent`](../../../crates/rimz/src/agents/lifecycle/event.rs) is the versioned public projection of one durable transition: event and workspace identity, agent lineage, the complete signal, the prior status, the resulting status and phase, the transition classification, and the compaction-closed and waiting-cleared facts. Store builds it at the same commit that appends `agent.lifecycle`, so an append that lifecycle policy suppresses produces no envelope, and a derived subagent append produces its own envelope in log order.

Hook ingestion dispatches real-edge envelopes through a static reactor table; state snapshots bypass it. Each reactor declares a [`SignalSet`](../../../crates/rimz/src/agents/lifecycle/event.rs) beside its action. Queued-message nudges, terminal run waits, and ended-agent message archival are reactors. They are latency paths and re-check durable state before acting.

[`rimz events follow`](../../reference/cli/events.md) reads the same vocabulary. The read-only [`store::follow`](../../../crates/rimz/src/store/follow.rs) follower folds real edges through `step` and emits one envelope per conforming real lifecycle record. State snapshots emit nothing, but update its private state through [`AgentState::snapshot_transition`](../../../crates/rimz/src/agents/state.rs), the same decision `AgentState::transition` calls. They preserve turn ids and compaction and clear the open ask, so the next real edge reports the updated prior status. The follower starts from a rollup seed at the live edge, or under `--replay` from empty state at the start of the current log generation, and drains a rotated tail before it reads the new active log.

## Displayed status

The rollup holds the agent-reported lifecycle status. `rimz sidebar snapshot` reports it in `snapshot.agents`, except that a current [local-session observation](./instances.md#local-session-observations) overwrites a row's status and phase there without touching the rollup (`crates/rimz/src/store/snapshot/view/live.rs::apply_local_lifecycle`). The displayed status is a projection over that status, computed in two layers.

[`effective_status`](../../../crates/rimz/src/agents/state.rs) is the cheap operational projection that message delivery, reply waits, keepalive, and idle stop use. The sidebar applies a separate display-rest projection for in-flight work. Effective status reads the rollup, the budget park, pending waits, and the context sidecar's markers, in this order:

1. A `waiting` agent whose keyless ask was answered reads as `running` in the `reasoning` phase: `waiting_since` is present, `last_activity` is newer, and no interruption marker applies. Every later step sees it as `running`. A keyed ask, or a wait without that evidence, stays `waiting`. A local-session observation that still reports `waiting` stamps the ask no earlier than its last observed write, so that write cannot pass for an answer. A later activity heartbeat can still release it.
2. A `running` row with a native-wait or plan-proposal marker reads `waiting`.
3. A row with a budget park, unless it is `waiting`, reads `paused`.
4. A `waiting` row with an interruption marker reads `idle`.
5. A `running` or `failed` row with a qualifying turn-error marker of a pausing class reads `paused`.
6. A `running` row with a completion marker reads `success`, with an interruption marker `idle`, and in the `parked` phase `success`.
7. A pending wait makes an `idle` or `success` result read `sleeping`.

That pending wait also holds a supervised run open at a clean turn end. Agent status and run status stay separate: a sleeping card can have a `Running` run with `parked_at` set.

A marker counts only when it is newer than `last_activity`, so any newer hook event clears it. The exception is a `failed` row's turn-error marker, which counts when `turn_started_at` is present and the marker is at or after it. For legacy sidecars, [`effective_turn_error_class`](../../../crates/rimz/src/agents/state.rs) displays a `failed` marker as a spend- or rate-limit park when its label matches that limit; a Console usage-cap label stays `failed`.

A turn-class change must check card and nested-child status and attention (including park demotion), operational status, agent and subagent reports, [doctor's account attribution](../../../crates/rimz/src/cli/doctor/agents.rs), [auto-continue](../../../crates/rimz/src/harness/auto_continue.rs) (limit handling through `TurnErrorClass::is_limit`, plus overload retries), [subagent park notices](../../../crates/rimz/src/harness/park_notice.rs) (`is_limit`), [stall notices](../../../crates/rimz/src/harness/stall_notice.rs) (suppressed by any displayed turn error), [workflow attempts](../../../crates/rimz/src/cli/workflow/host.rs), Codex's [pane-warning recognition](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) (`is_limit`), and the [headless judge's](../../../crates/rimz/src/harness/workflow/script/judge.rs) `limited` outcome.

### The sidebar ladder

The sidebar row projection, [`project_display_status`](../../../crates/rimz/src/store/snapshot/view/aggregate/status.rs), adds liveness, children, and provider budget windows on top. The first rung whose condition holds decides the displayed status:

| Rung | Condition | Displays |
| --- | --- | --- |
| 1 | a `waiting` row with neither an interruption marker nor proof its keyless ask was answered | `waiting` |
| 2 | a `running` row with a native-wait or plan-proposal marker | `waiting` |
| 3 | a budget park | `paused`, with spend against cap |
| 4 | a turn-error marker of a pausing class | `paused`, with the upstream label; `failed` with the same label once retries are exhausted or the limit window reset with no spent window |
| 5 | a turn-error marker of a fatal class | `failed`, with the upstream label |
| 6 | a live launched child with a displayed pausing marker under an `idle`, `success`, or `running` parent | `paused`, or `failed` after the child's spent-park demotion, labelled `@<child>: <label>` |
| 7 | a displayed `running` subagent under an `idle`, `success`, or `running` parent | `running` |
| 8 | a completion marker (a turn finished without a `Stop` hook) | `success` |
| 9 | an interruption marker (a turn or ask cancelled without a terminal hook) | `idle` |
| 10 | a clean turn parked on background work | `success`, keeping the `parked` phase |
| 11 | identical consecutive tool calls reach the attention threshold | `failed`, labelled `loop: <tool> ×<count>` |
| 12 | a `running` row silent past the stall window | `paused` when the kind has a spent window, otherwise `failed` |
| 13 | none of the above | the rested lifecycle or context status |

A `waiting` row that fails rung 1 first resolves by the shared waiting rule. With an interruption marker it becomes `idle` and continues down the ladder, skipping rung 3. An answered keyless ask continues as `running` in the `reasoning` phase until a durable clear lands. The interruption check uses the row's child-folded activity clock, while proof of an answer uses the source agent's own activity. A wait without `waiting_since`, or without a source agent, holds at rung 1 unless interrupted. An answered row is eligible for either child rung after normalization, including a delegated park, and a raw `failed` parent is not. The [`displayed_status_precedence_ladder_holds`](../../../crates/rimz/src/store/snapshot/view/tests/status/stall.rs) test stacks the causes against each other, so a reordering fails the suite even when every single-cause test passes.

The rungs that need more than their row:

- Rung 2, native waits and plans. A provider marker says a native dialog is open without a durable ask. The Codex case is a `running` row whose completed planning turn rests on a rollout `Plan` item: the normal `Stop` hook records a durable plan ask, and this marker is the backstop when that hook was missed ([adapter_codex.md](./adapter_codex.md#plan-approval-marker)).
- Rungs 3 and 4, paused. No hook emits `paused`. A launch `budget` reads the session's cumulative live cost against a per-session runtime ledger, and crossing it sends Esc and stamps the park ([budget.md](../harness/budget.md)). The pausing turn-error classes ([`TurnErrorClass`](../../../crates/rimz/src/agents/context.rs)) are `paused_rate_limit` and `paused_spend_limit`, per-agent markers with an account-scoped budget decision, and `paused_overloaded`, which covers provider overload, serving capacity, 5xx-class failures, stalled streams, timeouts, and connection drops. The promotion to `failed` happens when auto-continue has exhausted its retries, or when a rate-limit marker outlives its window's reset with no spent window left to explain it ([providers.md](./providers.md#spent-windows-and-paused-rows)).
- Rung 5, turn death. A non-transient provider API error or an unclassified turn-death marker escalates to `!` at once, and the card quotes the upstream or derived label. For a `running` row the marker must postdate `last_activity`, so an explicit death certificate beats both live-child activity and the stall window. For a `failed` row the marker must fall inside the current turn, so an old marker never explains a fresh failure.
- Rung 6, delegated parks. [`launched_child_park`](../../../crates/rimz/src/store/snapshot/view/aggregate/subagents.rs) and the nested entry share [`AgentState::rowless_status`](../../../crates/rimz/src/agents/state.rs) with the CLI. It uses the same [`ParkDemotion::is_spent`](../../../crates/rimz/src/agents/park.rs) predicate as rung 4, against the child's retries and login windows. The entry keeps the bare error label (or the class words), and the parent prefixes it with the child's handle. A demoted failure outranks a paused child, and equal states choose the earliest `registered_at`, then id. Ended children, native children, and ordinary fatal child errors do not propagate a park. The parent's own waiting, budget park, pausing marker, or fatal marker wins first.
- Rung 7, waiting on children. A displayed `running` child, native or launched, makes the parent paint a quiet wave. A stalled child of either origin does not hold the parent's turn. The stall clock reads the row's displayed activity, which folds in the children's, so a child that just finished defers escalation too. The durable resting status returns after the last child stops, unless a raw-running parent takes its own stall rung.
- Rung 8, completion. Codex `/review` ends on a clean rollout `task_complete` with a non-empty `last_agent_message` and no `Stop`. The marker settles the row instead of letting the stall window misread a finished review ([adapter_codex.md](./adapter_codex.md#turn-completion-marker)).
- Rung 9, interruption. Codex writes rollout `turn_aborted` for Esc and for `/clear` mid-turn ([adapter_codex.md](./adapter_codex.md#turn-interruption-marker)). Claude writes a transcript `user` entry beginning `[Request interrupted by user` ([adapter_claude.md](./adapter_claude.md#turn-interruption-marker)).
- Rung 11, tool loops. A looping agent completes tools and refreshes its heartbeat, so it can never stall. A run of the same tool name with the same canonicalized arguments reaching `[agents.attention] tool_repeat_attention_after` (20 by default) is the progress-failure certificate. The next differing tool or other progress clears the run without human action.
- Rung 12, stall. The backstop for any other silent `running` row, after `[agents.attention] stalled_after_secs` (30 minutes by default). Its rules for children follow this list.

The stall rung also covers children. Any open native or launched child takes the same silence check when it has no displayed error and is not sleeping or paused. A prompted launched child at raw and effective `idle` with no `turn_ended_at` qualifies as well, because it has not started its first turn. Its carried `prompt` tells it apart from a promptless interactive launch, since `task` may be cleared at registration and `turn_started_at` is already stamped by the prompted launch and restamped by registration. The child's entry becomes `failed`, labelled `silent Nm`, with runtime frozen at its last activity. It stops holding the parent's turn, so a silent raw-running parent can fail in the same fold. A resting parent keeps its own status. A native child's failure lifts its attention, because the child has no pane, while a launched child's silence does not. Only launched children get a parent notice. The next heartbeat clears the stall projection.

Each rung leaves the rollup's lifecycle status untouched. Claude transcript-death can leave the rollup `running` while Codex's `Stop` over a rollout error records it `failed`, and the projection displays either as `paused` when the error class pauses.

A projection to any status other than `running` drops the phase, except `success` or `sleeping` over `parked`, the settled shapes that keep pending background work visible. The compaction head pulses over any displayed status.

### Sleeping

After the ladder settles, [`AgentState::display_rest_status`](../../../crates/rimz/src/agents/state.rs) lifts an `idle` or `success` result. A Working-class wait or a non-empty `background_shells` list lifts it to `running`, and otherwise a Sleeping-class wait lifts it to `sleeping`. Inert waits contribute nothing. Attention statuses and live-child delegation keep precedence. The lift follows the stall rung, so a resting row working through a wait or shell does not stall on its own quiet clock, while a foreground turn still does. A parked clean turn keeps its `parked` phase, but that phase alone proves no shell is running.

Enrichment reads pending waits from the loop catalog without writing a store event. A row counts when it is instance-sourced and one-shot, and matches the workspace root and the target kind and session. Timers, signals, conditions, and file watches are Sleeping. PIDs, watched commands, and polled checks are Working while their catalog row exists, and the elder consumes lost watches after its watcher grace window. Standing subscriptions and recurring clocks do not count, even when a recurring row has a deadline.

A pending catalog wait needs a parsed trigger. A self-wait timer's due time anchors at `wait_meta.armed_at`. A human one-shot `loop add --wait --at HH:MM` row has no arm timestamp, so its due time is the next occurrence after the snapshot clock in the configured timezone.

Enrichment also reads the runs directory when launched fleet members or agent-launched team seats exist. It prepends a `Subagent` wait per unsettled launched child or peer run, then a `Team` wait per open team run belonging to the launcher. A Team wait is Working until the board is Done. A terminal Subagent wait is Inert, and an open one follows its target's turn, wait chain, and shells. A silent running child contributes no work, and a cycle is Inert. These waits and their classes exist only in enrichment, never as catalog rows or store events. The [keepalive rules](../harness/messaging.md#prompt-cache-keepalive) define settlement and the resulting pings. Operational `effective_status` still treats every pending one-shot wait as Sleeping, so delivery and wake accounting see one class.

`sleeping` is neither attention nor actionable, so `needs_a_look` is false. The unread episode and any configured success notification open when the wait cycle finishes at `success`, never on the intermediate rest, and an existing unread episode persists across sleep. The cockpit tally orders `waiting → failed → paused → success → running → sleeping → idle`.

Delivery and maintenance read the operational status. The `Done` and `Any` delivery gates open for an operationally sleeping agent, even when its card displays working, so a message can start a turn without consuming the armed wait. `Resume` does not open, and reply waits do not treat sleep as completed. Operationally sleeping agents are not idle-compacted, and eligible unfrozen sleepers receive prompt-cache keepalive pings instead. `--when` still reads the raw lifecycle status and does not accept `sleeping`, and `agent.idle` stays a turn-boundary event.

[Hibernation](../harness/hibernate.md) attaches metadata without changing displayed status, ranking, unread episodes, notifications, or turn-completion waits. A frozen resting row with a sleeping wait still reads `sleeping`, and frozen rows receive neither cache keepalive pings nor idle compaction.

## Activity clocks

`last_activity` is the session's own latest event, never inherited from another instance. Named or mutating tool observations can enter the durable log, and anonymous tool progress need not. Hook ingestion also touches a per-session runtime heartbeat ([`agent_activity`](../../../crates/rimz/src/agent_activity.rs)) on progress-proving events (tool observations, turn boundaries, subagent start and stop), and the snapshot folds the freshest touch into `last_activity`. The heartbeat stamps its progress and tool clocks at hook ingress time, not apply time, and neither clock moves backward when frames apply out of order. A delayed progress hook therefore cannot hide a newer provider-limit marker.

Which events touch which clock depends on the session. For an own-session agent, including a launched child, a pre-tool event advances only the heartbeat's tool clock (`tool_at`), leaving `last_activity` and the repeated-tool run alone. Native child-keyed hooks also advance `last_activity`, pre-tool included, at whatever cadence the provider emits. A blocked root wait touches nothing. The heartbeat is keyed by the event's own session, so a background subagent's progress touches the child's heartbeat, and a parent blocked on an ask keeps its clock frozen until it acts.

The heartbeat keeps a busy row animating, drives the [stall rung](#the-sidebar-ladder), and releases a keyless ask answered in the pane once `last_activity` passes `waiting_since`. It is latency, not truth: a missing heartbeat file leaves `last_activity` at the event-log timestamp.

[`AgentState::last_request_at`](../../../crates/rimz/src/agents/state.rs) is the cache timers' request anchor. For a closed turn it takes the later of `turn_started_at` and `last_tool_at`, clamps that to `turn_ended_at`, and returns the later of the result and `pinged_at` ([keepalive pings](#keepalive-pings)). With neither a turn end nor a ping it is `None`. `last_tool_at` is non-durable enrichment from the heartbeat's `tool_at`, set on root `ToolUsed` observations (anonymous tools included) and preserved across other touches such as Stop. The fold keeps it even when a newer durable event already advanced `last_activity`. A restart restamps the turn start but not its end, so the clamp keeps the prior end. A successful compaction close that is not ignored stamps `turn_ended_at`, and a successful close that lands at rest after a turn also restamps the start. Without per-tool hooks the anchor falls back to the turn start, an earlier estimate. These clocks are best-effort cache hints, never correctness state.

Estimated active time accumulates a provider session's observable work. Hook ingestion opens or advances a working span on turn starts, tool progress, and compaction progress. It freezes the span on waits, turn ends and interruptions, parked background work, session exit, and provider-error markers. An open span extends past its latest progress signal by at most `[agents.attention] active_grace_secs` (180 seconds by default), so a silent process stops accruing instead of fabricating work. Later progress credits that capped tail and resumes without bridging the gap.

Each `(kind, agent_id)` active-time record lives under the runtime `active-time/` directory, updates under a per-record flock, and publishes by atomic rename, so the accumulator survives sidebar and RimZ restarts while the room runtime lives. Attribution, the sidebar cohort total, and the agent card and list read the shared seat sum, launched children included. Sidebar rows carry no active-time clock. A nested subagent carries start-to-now runtime while running, frozen at its last activity on a finish or stall, and an open paused child's runtime keeps counting.

## Enrichment

The store and explicit events decide routing, ranking, and state, and enrichment paints the row. `task` is display-only and redactable. The `usage` figures (`context_pct`, `context_window`, `total_tokens`) are redactable and also feed compaction policy: the smart-compact threshold of a queued message (`crates/rimz/src/store/message.rs::AutoCompact::triggered`) and idle-compaction eligibility (`crates/rimz/src/harness/idle_compact.rs::eligible_seat`). A missing value means the agent did not report it, never zero, and an unknown fill never triggers a compaction. The sidebar still draws an unreported context gauge at a visible 0% baseline.

`context_window` is the model's maximum input tokens for every agent. The gauge's numerator counts input-side occupancy only, input plus cache and never output ([`context_used_tokens`](../../../crates/rimz/src/agents/state.rs)), so a model with separate input and output caps scales against the input cap. Each adapter resolves the window its own way: Claude from the payload model id, where `[1m]` widens it, Codex from the rollout's `model_context_window`, and OpenCode from its model catalog. The card's identity line renders it (`258k`, `1M`), preferring a fresher reading from rich context when one exists. The context sources and their reading rules are [adapter.md](./adapter.md#context-sources). These are bare token counts, so `payload_mode`, which gates the content of high-frequency payloads, never hides them.

### Rich context

Some agents publish far more per-session data out of band than their hooks carry: context-window accounting, the latest message's usage breakdown, cost, rate-limit windows, model display name, thread preview, PR info, version, and effort. `observe_context` normalizes each transport's payload into the provider-neutral [`AgentContext`](../../../crates/rimz/src/agents/context.rs). Apart from the required `source` and `observed_at`, every field is an `Option` or a defaulted list parsed tolerantly, so a sparse or newer payload still parses and the renderer draws whatever is present. The account and balance subset (plan, metered state, rate-limit windows) feeds the provider dashboard ([providers.md](./providers.md)). `AgentContext` carries no account fingerprint, so a sidecar rewritten on every render can never change which account a session's windows count against. That stays with the rollup's `account_key`.

Rich context is high-frequency and display-only, so it never rides the event log. CLI producer paths (the statusline feed, hook ingestion, detached refresh helpers, and the Codex stat-gated backstop) write a latest-wins sidecar, one atomic file per `(kind, agent_id)` under the runtime `agent_context/` directory, and `rimz sidebar snapshot` folds each record onto its `AgentState`. The directory sits under the per-user runtime root (mode `0700`), the same exposure as the heartbeat and diff-stats caches. A session-end event removes the sidecar, and `rimz gc` sweeps old files.

A few `AgentContext` fields reach past display: the turn-error, turn-settle, and native-attention markers feed [`effective_status`](#displayed-status) and the sidebar ladder. That is how every read path agrees about state no hook reported, without inventing durable records.

## See also

- [adapter.md](./adapter.md): where observations come from, the hook path, and context sources.
- [instances.md](./instances.md): binding sessions to panes, pane ownership, and session death.
- [attribution.md](./attribution.md): durable effort credit folded from the audit rollup.
- [providers.md](./providers.md): accounts and balances.
- [spending.md](./spending.md): spend and pricing.
- [sidebar.md](../sidebar/sidebar.md): presence binding, ranking, and how the rollup becomes a row.
- [store.md](../store.md): the durable event log the rollup replays.
