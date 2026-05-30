# Agent integrations

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Agent integrations are adapters that translate a coding agent's native hook protocol onto Rimz events and feed items. The generic event/feed API is the ground truth; agents are sources. Anything an agent integration does, a shell script can do through the same CLI.

This doc owns the *how Rimz knows what an agent is doing* layer end to end: the unified adapter interface, the normalized observation every adapter emits, the attribute catalog, the agent state machine, and liveness. [sidebar.md](./sidebar.md) owns what sits above this — presence, worktree grouping, ranking, and rendering. The seam is the snapshot: this doc produces the per-agent `AgentState`; the sidebar projects it.

## The unified interface

Every agent — Claude, Codex, and every future one — speaks to Rimz through **one trait**, [`AgentIntegration`](../../crates/rimz/src/agents/mod.rs). Adding an agent is implementing the trait; nothing downstream of it is agent-specific. The trait is the single place native protocol diverges and the single place it is normalized:

```text
install_hooks() / uninstall_hooks() / hooks_installed()   — own the per-user config write
classify_hook(event, payload)   → lifecycle | blocking-feed | unknown
observe_lifecycle(event, payload) → Option<AgentLifecycleObservation>   — the normalized event
render_decision(feed_kind, resolution) / render_neutral(event)          — agent-native stdout
hook_cap() / ends_session(event)
```

Two outputs carry everything downstream needs, so the rest of Rimz never reads a native payload:

- **`classify_hook`** routes a native event into one of three lanes (§ Two hook channels). Blocking-feed lane → a `FeedItem`; lifecycle lane → an observation; unknown → ignored.
- **`observe_lifecycle`** is the normalizer. It maps a native event onto a single [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs) — the **unified event shape**. Returning `None` means "this event carries no state transition", so high-frequency hooks stay silent. Every field an observation can carry is in the [attribute catalog](#attribute-catalog).

The observation is the contract boundary. `EventEnvelope::agent_lifecycle` serializes it to the event log; `reduce_agent_states` folds the log into one [`AgentState`](../../crates/rimz/src/feed.rs) per running agent — the **unified global state** the sidebar paints. A new agent that emits well-formed observations gets the state machine, ranking, liveness, and jump for free.

Decision renderers stay agent-specific. Do not reuse one agent's JSON shape for another. Claude and current Codex both use `hookSpecificOutput` for `PermissionRequest`, but Codex rejects fields such as `updatedInput`, `updatedPermissions`, and `interrupt` on that event.

## Two hook channels

`classify_hook` sorts every native event into one of two wired channels (plus `unknown`, dropped). The distinction is whether the hook can hold the agent open while Rimz waits for an answer.

**Lifecycle hooks — fast, non-blocking.** Drive agent status, permission posture, notifications, enrichment fields, and the audit history in `rimz feed list --audit`. They flow through `observe_lifecycle`.

```text
SessionStart   UserPromptSubmit   PreToolUse   PostToolUse
Stop           SessionEnd         Notification
```

**Feed hooks — blocking-capable.** The path the bridge engages; they become a `FeedItem`, not an observation.

```text
permission request
plan approval
user question
```

Blocking decision hooks must be **sync**. Installing one as async is a hard error — the agent would ignore the decision printed on stdout. The installer rejects async configs explicitly.

## The unified global state

`reduce_agent_states` folds the lifecycle observations into one `AgentState` keyed by `(kind, agent_id)`. Each event is a *partial* update: `status` always comes from the event, capability fields carry forward, and activity fields are replaced. The result is the agent row the sidebar projects.

### Attribute catalog

Each field, where it comes from, and its **lifetime** — the rule the reducer follows when an event omits the field:

- **identity** — established once at session start, stable for the session.
- **activity** — replaced by the latest event; clearing it is meaningful (an idle agent has no task).
- **carry-forward** — capability/enrichment that persists until a newer value arrives; a missing value never resets it.
- **live-derived** — not stored in the ledger; computed at snapshot time from the live pane list or git (see [sidebar.md → Presence model](./sidebar.md#presence-model)).

| Field                               | Meaning                           | Source (event · payload field)                                    | Lifetime      |
| ----------------------------------- | --------------------------------- | ----------------------------------------------------------------- | ------------- |
| `agent_id`                          | session/instance key              | `session_id` (Claude); `agent_id`→`session_id` (Codex)            | identity      |
| `parent_agent_id`                   | root session of a subagent        | `SubagentStart`/`SubagentStop` · `session_id` (both agents)       | identity      |
| `kind`                              | `claude` / `codex`                | `--source` on the hook                                            | identity      |
| `status`                            | 5-value rollup (below)            | derived from `event_name` (§ state machine)                       | activity      |
| `permission_posture`                | `default`/`plan`/`auto`/`yolo`/`unknown` (`plan` → thinking) | every lifecycle event + per-tool heartbeat · `permission_mode`/`approval_policy`, last sample wins (§ Plan mode as a sticky posture) | carry-forward |
| `task`                              | what it's working on              | `UserPromptSubmit` · `prompt`; `SubagentStart` · `agent_type`     | activity      |
| `model`                             | `Opus`, `GPT-5.5`                 | lifecycle · `model` (canonicalized — § below)                     | carry-forward |
| `effort`                            | `xhigh`/`high`/…                  | lifecycle · `thinking_level`/`model_reasoning_effort`             | carry-forward |
| `context_pct`                       | context-window % gauge            | payload or transcript tail (§ enrichment)                         | carry-forward |
| `total_tokens`                      | cumulative tokens                 | payload or transcript tail                                        | carry-forward |
| `todo_done`/`todo_total`            | plan progress dots                | Claude `TodoWrite` · `tool_input.todos`; Codex none today         | carry-forward |
| `agent_pid` / `agent_process_start` | liveness gate                     | `RIMZ_AGENT_PID=$PPID`, else `/proc` ancestor walk                | identity      |
| `runtime_owner`                     | owner-process identity            | built from `agent_pid` + start token                              | identity      |
| `worktree_path` / `worktree_branch` | grouping spine                    | live pane cwd → worktree (ledger value is detached-only fallback) | live-derived  |
| `pane`                              | jump target                       | bound live at snapshot from the pane list                         | live-derived  |
| `last_activity`                     | age + attention rank              | `event.timestamp`, advanced per tool by the activity heartbeat    | activity      |
| `last_seen`                         | carryover-merge tiebreak          | `event.timestamp`                                                 | activity      |

The catalog turns on one distinction: **identity vs. live-derived**. `worktree_*` and `pane` are *live* facts — the pane knows its current cwd every tick — so they are derived at snapshot time, not pinned at session start. Pinning them is the branch-tracking bug (§ Liveness and presence).

The reducer stores `model` **canonicalized** — a trailing capability tag is stripped (`claude-opus-4-8[1m]` → `claude-opus-4-8`). The tag rides only on a fresh-launch `SessionStart` payload: it is absent after `/clear` (a new `agent_id`), the transcript records the bare id, and no model env var exposes it. So a suffix-less follow-up event plus the `model` carry-forward would flip the label `…[1m]` → `…` the first time it arrived. Canonicalizing at reduce time pins one stable id while the event log stays faithful to the raw payload.

### The state machine

The five-value status set, in ranking order (most attention-hungry first — a working `running` agent is the least, so it sorts below the calm-but-settled `idle`/`success`), per [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape), which owns the full glyph/animation/color table:

| Status    | Glyph | Meaning                     | Raises attention |
| --------- | ----- | --------------------------- | ---------------- |
| `waiting` | `?`   | blocked on a human decision | yes              |
| `failed`  | `!`   | the last turn errored       | yes              |
| `idle`    | `◌`   | wired in, nothing in flight | no               |
| `success` | `✓`   | last turn completed cleanly | no               |
| `running` | `⢿`   | actively working a task     | no               |

The displayed cell refines `running` two ways without changing the rollup: a `running` agent whose permission slider is in `plan` renders as **thinking** (`✽`, a sparkle animation — the `plan` posture below), and a `running` agent silent past the stall window escalates to the attention **`!`** (see [Liveness and presence](#liveness-and-presence)). A working `running` agent animates a braille spinner; the resolver-mid-flight overlay animates a braille spinner. Only these active states animate — `?`, `!`, `◌`, `✓` are static so attention stays scannable.

`waiting` is **not** a lifecycle transition. It is the presence of a pending blocking feed item joined to the agent (the feed channel, not the lifecycle channel). The lifecycle machine drives the other four:

```text
   (none) ──SessionStart──► idle ──UserPromptSubmit / SubagentStart──► running
                             ▲                                          │
              next prompt    │   Stop(clean) ──► success                │
              re-enters ─────┤   Stop(error) ──► failed   ◄─────────────┤
              running        │   SubagentStop ──► idle (child)          │
                             └──────────────────────────────────────────┘
   blocking ask pending while running ──► waiting (feed channel, not lifecycle)

   any state ── SessionEnd / pid dead / pane reverted to shell ──► removed (no row)
```

`Stop` only fires after a turn ran, so it resolves the turn — `success`, or `failed` on an explicit error signal — never back to `idle`. One exception keeps it `running`: a `Stop` whose payload carries in-flight `background_tasks` (Claude Code v2.1.145+) is the main thread parking, not a turn end — it reawakens when the background work reports back — so the row stays `running` and labels itself with that work rather than painting a false `success`. An error still wins (the failure is the attention signal). `idle` is the resting state `SessionStart` establishes and a finished `SubagentStop` child returns to; `success`/`failed`/`idle` all re-enter `running` on the next prompt.

The agent owns status and posture; Rimz observes and renders. `yolo` is observed from the agent's own bypass flag (`claude --dangerously-skip-permissions`, `codex --ask-for-approval never`). `plan` is a first-class read-only posture (rendered as thinking while running); `interactive` folds into `default`. The vocabulary is defined once in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape).

#### Plan mode as a sticky posture

`plan` is one position of the permission slider, not a separate flag: `thinking` is `running` joined to `permission_posture == plan`, and the sparkle paints only while the agent is `running`. The slider is *sticky* and present on every hook (`Stop` included), so Rimz treats it as one **last-sample-wins** value — every lifecycle event and every per-tool activity heartbeat samples `permission_mode`/`approval_policy`, an event that names no slider carries the prior value forward, and the freshest sample wins. There is no turn-boundary special-case and no approval-driven clear, because the slider is self-correcting:

- **Approving a plan** moves the slider off `plan` (Claude switches to `default`/`acceptEdits`), so the agent's next hook — a `Stop`, the next prompt, or the next `PostToolUse` — reports the new posture and the sparkle drops on its own.
- **Shift-tabbing out of `plan`** mid-turn raises no lifecycle event, only per-tool hooks. The activity heartbeat carries that per-tool slider reading to projection time, where `with_agent_activity` applies it as a last-sample-wins override (guarded `> last_seen`, the agent's latest lifecycle event, so a prior turn's touch can't fire). The heartbeat is keyed per `agent_id`, so a subagent's non-plan tool touches its own leaf-session heartbeat and never clobbers the parent's `plan` posture — the bug that made a planning parent render as `working`.
- **A no-tool turn** fires no per-tool hook, so the posture changes only at the next lifecycle event (`Stop`/next prompt) — a brief, bounded latency, never a stale latch across turns.

### Instance identity and age

An agent row belongs to **one running instance**. The key is `(kind, agent_id)` with `agent_id` the agent's session id, so two concurrent agents of the same kind never share a row and `last_activity` is always the agent's *own* latest event — never inherited from a previous instance of the same kind.

When a payload carries no session id, the adapter keys on the captured `runtime_owner` (pid + start token) rather than a shared anonymous bucket, so two unidentified instances never merge; a truly unkeyable event is dropped rather than collapsed — better no row than a row that lies about its age.

### Enrichment is display-only

`task`, `context_pct`, `total_tokens`, and the todo counts are **enrichment**: display-only, redactable, and they never drive routing, ranking, or a decision (the no-transcript-correctness rule). The reduced agent state keeps missing context as "the agent didn't report it"; the sidebar row projects that missing value to a visible 0% baseline so every observed agent has a context bar.

Context budget is the one field no agent puts directly in its hook JSON — usage lives in the transcript. Capture reads the **transcript tail** only after the agent payload supplies a `session_id`, on the low-frequency events Rimz already fires (`SessionStart`, `UserPromptSubmit`, `Stop`), takes the most recent assistant usage record, and scales it against the model's context window, so the gauge upgrades without a per-tool hook. These are bare token counts (metadata); `payload_mode` gates the *content* of high-frequency event payloads, never these gauges or the *state transition* they ride on.

### The statusline as a context datasource

Some agents publish far richer per-session data out of band than their hooks carry. Claude `exec`s a configured `statusLine` command on every render and pipes a JSON blob to its stdin — the user's `session_name` (when set), the context-window token accounting including the most-recent message's `current_usage` breakdown (`input` / `cache_creation` / `cache_read` / `output`), cost, rate-limit windows with reset instants, PR info, vim mode, version, output style, effort, and `exceeds_200k`. `observe_context` normalizes any such transport into the agent-agnostic `AgentContext`. Codex has no statusline; it produces the same `AgentContext` from the **Codex app-server** out of band (see the [Codex appendix](#appendix--codex)) — same record, same storage and fold-in. Every field is `Option` and tolerantly parsed — `current_usage` is null before the first API call and after `/compact`, `rate_limits` is absent for non-subscribers, `session_name` is absent until named — so a sparse or evolved payload always parses, and the renderer draws whatever subset is present.

This is high-frequency, display-only enrichment, so it does **not** ride the event log. The feed process writes a **latest-wins per-session sidecar** — one atomic file per `(kind, agent_id)` under the runtime `agent_context/` dir — and `rimz sidebar snapshot` folds each record onto its `AgentState` by session key (`with_agent_context`). The sidecar lives wholly off the durable path: routing correctness stays in the ledger ("Ledger first — sidebar wakeups are latency, not truth"). It dies with the session — a `SessionEnd` tombstones it, and a read past the ghost-session TTL drops it even if that tombstone was missed.

`AgentContext` is metadata and gauges of the same class as `context_pct`/`total_tokens`, so it is not gated by `payload_mode`. When a `payload_mode` loader lands, gate only the content-ish fields (`pr.url`, `output_style`, `vim_mode`); the numeric gauges, cost, and rate-limit windows stay always-on. The sidecar lives under the per-uid runtime root (mode `0700`), no broader exposure than the heartbeat or diff-stats caches.

## Liveness and presence

Presence comes from the live pane list, not from a session-exit hook (see [sidebar.md → Presence model](./sidebar.md#presence-model)). The binding is exact: every lifecycle event stamps the mux's own per-pane env var (`TMUX_PANE` / `ZELLIJ_PANE_ID`) — the mux's ground-truth pane assignment, not an agent self-claim — and the snapshot binds each live pane to the one agent that stamped that exact id. An agent renders only on its stamped pane; one whose pane reverts to a shell, closes, or is otherwise absent from the live list is gone, with no exit event required.

The captured pid is **not** a render gate — stamped-pane binding already keeps a stale agent off a stranger's pane. It feeds the rollup's hygiene instead: on a lifecycle event Rimz records the agent's pid best-effort (`RIMZ_AGENT_PID=$PPID`, falling back to a `/proc` ancestor walk, plus the Linux process-start token to defeat pid reuse), and the reaper below reads *pidless* as one of its ghost signals.

There is no `offline` status — a dead agent is a reverted shell row or no row at all, never a retracted ledger fact.

**Per-tool activity is a heartbeat, not an event.** The durable event log is turn-grained — `last_activity` would otherwise advance only on `SessionStart`/`UserPromptSubmit`/`Stop` — so the hook touches a per-agent activity heartbeat (`runtime/agent-activity/`, the [`agent_activity`](../../crates/rimz/src/agent_activity.rs) module) on every progress-proving event (`PostToolUse`, the turn boundaries, subagent start/stop), and the snapshot folds the freshest touch into `last_activity`. It is **not** touched on `PreToolUse` (which can fire in the same tool call as a blocking ask) or while the agent is blocked. This per-tool signal does three things: it keeps a busy agent's row animating (the spinner tracks real work, not a stale 4-second window), it escalates a `running` agent silent past the ~10-minute stall window to the `!` attention state, and it recovers an answered `native_ui` ask — the snapshot stops folding an ask onto the row once `last_activity` passes the ask, so an agent that answered in its own UI and kept working returns to `running` without waiting for the next turn boundary. Like every heartbeat, it is latency, not truth: a missing or stale file just leaves the event-log timestamp.

**The rollup reaps its own ghosts.** A session that never captured a pid and never fired `SessionEnd` would otherwise pin a stale row forever, and relaunch-in-place or shared-pid sessions stack duplicates. At snapshot time the derived rollup (never the event log) drops two classes, both safe for one-pane-one-row: a *pidless* session past a few-hours TTL, and an *older* session superseded by a strictly-newer same-kind session on the same `(worktree_path, worktree_branch)` when the older holds no live pane the newer doesn't already occupy. An agent holding its own distinct pane is always kept. This is workspace-local and complements the cross-workspace `rimz gc`.

Two consequences this contract enforces (status in [Implementation status](#implementation-status)):

- **A pane the agent never stamped is a process row.** Command and cwd never bind a row, so after an agent exits and `git log` (or a fresh `node`) runs in the same or a neighbouring pane, that pane has no agent that stamped it and stays a process row. Two same-kind agents in one worktree — indistinguishable by command and cwd — bind only to their distinct stamped panes and never cross-wire.
- **Worktree and branch track the live pane.** Branch and worktree are resolved from the pane's current cwd at snapshot time (the same place diff stats are read), so they follow `git checkout` and a pane `cd` into another worktree. The ledger's pinned `worktree_*` is a fallback only for a detached agent with no live pane.

Pane binding and jump are the snapshot's job, documented in [sidebar.md → Jump](./sidebar.md#jump--the-row-is-the-link): binding is by the stamped pane id alone — no command or cwd fallback — and every jump reconciles pane id *and* `pane_process_start` so a reused id never focuses a stranger.

## Hook install is an explicit, visible step

Hook install is a security surface. `rimz start` detects installed, supported agents each run, previews the additive per-user config change, installs missing hooks when approved, and continues without installing when the user skips or declines. `rimz hooks install <agent>` is the manual entry point. An agent run before hooks are installed fires no hook and registers nothing. `hooks_installed()` makes that state observable: `rimz doctor` reports it per agent, and the sidebar's first-run hint points at `rimz hooks install` until an agent is wired (see [sidebar.md → Empty-room hint](./sidebar.md#empty-room-hint)).

### What install wires

Install wires every event the **state machine** needs plus the high-frequency, content-heavy per-tool hooks that keep the sidebar's enrichment current.

```sh
rimz hooks install claude
```

`UserPromptSubmit` and `Stop` are state signal — without them an agent never enters `running` and never carries a task. `PostToolUse` and broad `PreToolUse` fire on every tool call and carry tool inputs, prompts, file paths, and outputs; they drive real-time enrichment and `rimz feed list --audit` depth. Gate their payload content against `[privacy] payload_mode`:

- `payload_mode = "metadata"` — strips inputs, prompts, args, errors. Smallest footprint.
- `payload_mode = "redacted"` — keeps bounded payloads with built-in redaction. Default.
- `payload_mode = "full"` — keeps hook payloads as delivered. `rimz doctor` warns.

Privacy gates the *content* of an event, never whether a state transition is observed.

### The installed config shape

Each event is its own key in the agent's config — neither Claude nor Codex has a wildcard event key, so install writes one block per wired event. Inside that constraint the config stays minimal:

- **One command for every event.** The installed command carries no `--event`: it is `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>` everywhere. The helper reads the event from the stdin payload's `hook_event_name` (the override flag `rimz hooks feed --event` survives only for manual debugging).
- **No matcher for the blocking pair.** Claude's `ExitPlanMode` and `AskUserQuestion` blocking hooks ride the broad `PreToolUse` hook — runtime classification routes by `tool_name`, so each still maps to its own feed kind. A dedicated matcher would only double-fire: Claude runs *every* matching matcher group, and the broad entry already matches those tools.
- **Idempotent and self-healing.** Install reclaims every rimz-owned entry — marked or not, with or without a legacy `--event` — by the stable command substring `rimz hooks feed --source <agent>`, then rewrites the canonical set. Duplicate or stale blocks left by older builds never accumulate. User-authored hooks (no rimz command) are untouched.

Install also manages the agent's statusline when it has one. For Claude it sets `statusLine` to `rimz statusline feed --source claude`; if the user already has a statusline command, install **wraps** it — Rimz captures the JSON, then passes it unchanged to the original command and forwards that command's stdout and exit code, so the rendering is visually unaffected. The user's original value is stored verbatim under a `_rimz_wrapped` marker on the `statusLine` object and restored on uninstall (or the field is removed if Rimz added it). The wrap is a visible security surface: the consent gate summarizes it and the full change is in the install diff. The feed path is ledger-free and lock-free — it runs on every render — and its child's stdio is fully piped, never inherited, so a wrapped command's stderr never leaks onto the statusline.

## Adding an agent

OpenCode, Pi, Cursor, Gemini, Copilot, Amp, Rovo, Hermes, Factory, Qoder, and similar agents land through `AgentIntegration` once their hook surfaces and decision outputs are verified. The work is a new appendix below — the native-event → unified-interface mapping — plus the trait impl. Nothing else changes.

Adding an agent requires tests for: install/uninstall, lifecycle mapping (native event → observation → state), feed classification, neutral stdout, decision stdout, PID attribution, and version drift behaviour. Pinned hook stdout shapes live as inline `insta::assert_*_snapshot!(... @"...")` goldens inside each adapter module — see [`claude.rs`](../../crates/rimz/src/agents/claude.rs) and [`codex.rs`](../../crates/rimz/src/agents/codex.rs).

---

## Appendix — Claude Code

The mapping from Claude's native protocol onto the unified interface. The appendix says only *which native events are wired* and *how each maps* — the behaviour they drive is the state machine above.

Native event → unified mapping:

| Native event                  | Channel       | `observe_lifecycle` → status        | Normalized fields                           |
| ----------------------------- | ------------- | ----------------------------------- | ------------------------------------------- |
| `SessionStart`                | lifecycle     | `idle`                              | posture, model, context/tokens (transcript) |
| `UserPromptSubmit`            | lifecycle     | `running`                           | `task` = prompt; refresh context/tokens     |
| `SubagentStart`               | lifecycle     | `running`                           | keyed by child `agent_id`; `parent_agent_id` = `session_id`; `task` = `subagent_type`/`description` |
| `SubagentStop`                | lifecycle     | `idle`                              | child row; keeps `task` (type label) and parent link; clear plan mode |
| `Stop`                        | lifecycle     | `success` (error → `failed`; in-flight `background_tasks` → `running`) | `task` = background work else clear; clear plan mode unless still `plan`; refresh context/tokens |
| `SessionEnd`                  | lifecycle     | removed                             | —                                           |
| `Notification`                | lifecycle     | none (silent)                       | —                                           |
| `PermissionRequest`           | blocking-feed | `waiting`                           | —                                           |
| `PreToolUse: ExitPlanMode`    | blocking-feed | `waiting`                           | plan approval                               |
| `PreToolUse: AskUserQuestion` | blocking-feed | `waiting`                           | user question                               |
| `PostToolUse`                 | lifecycle     | none                                | `TodoWrite` todos; context/tokens           |
| `PreToolUse` (broad)          | lifecycle     | none                                | audit depth                                 |

Decision shapes — Claude requires `hookSpecificOutput`:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
```

```json
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {} } }
```

`ExitPlanMode` and `AskUserQuestion` require `updatedInput`. The Claude adapter sets `hook_cap = 120s` (upstream cap ~125s; Rimz leaves a 5s margin so the bridge times out before the agent kills the hook). The exact value is `CLAUDE_HOOK_CAP` in [`claude.rs`](../../crates/rimz/src/agents/claude.rs). Install merges non-destructively into `~/.claude/settings.json` under per-matcher `_rimz_managed` markers; `PreToolUse` installs as a single broad hook whose blocking sub-events self-classify from `tool_name`, and only `PermissionRequest` carries `_rimz_sync = true` (see [The installed config shape](#the-installed-config-shape)).

Claude Code routes `Task`-tool children through `SubagentStart`/`SubagentStop` (parity with Codex threads). A subagent event carries the child's `agent_id` and `agent_type`/`subagent_type`; Rimz keys those rows by the child `agent_id`, so the child gets its own `AgentState` rather than overwriting the parent session's, and captures the payload's `session_id` as `parent_agent_id`. The sidebar nests the child under its parent row (see [sidebar.md → Sub-agent lists](./sidebar.md#sub-agent-lists)); the child's type rides `task` on both events so a finished child stays labeled while it lingers in the parent's expanded list.

## Appendix — Codex

| Native event                                          | Channel       | `observe_lifecycle` → status | Normalized fields                                |
| ----------------------------------------------------- | ------------- | ---------------------------- | ------------------------------------------------ |
| `SessionStart`                                        | lifecycle     | `idle`                       | posture, model, effort                           |
| `UserPromptSubmit`                                    | lifecycle     | `running`                    | `task` = prompt                                  |
| `SubagentStart`                                       | lifecycle     | `running`                    | keyed by child `agent_id`; `task` = `agent_type` |
| `SubagentStop`                                        | lifecycle     | `idle`                       | child row; clear task                            |
| `Stop`                                                | lifecycle     | `success` (error → `failed`) | clear task                                       |
| `PermissionRequest`                                   | blocking-feed | `waiting`                    | —                                                |
| `PreToolUse`/`PostToolUse` (broad)                    | lifecycle     | none                         | audit depth                                      |

Decision shape — Codex permission hooks emit only `hookSpecificOutput.decision`:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
```

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "deny", "message": "Blocked by repository policy." } } }
```

Never emit `updatedInput`, `updatedPermissions`, or `interrupt` for Codex permission hooks — those fields belong to other Codex hook types and corrupt the permission decision. Codex's hook cap is shorter than Claude's (`CODEX_HOOK_CAP`); chain budgets must account for it. Install writes inline `[[hooks.Event]]` tables in `~/.codex/config.toml` with the same `--event`-free command and substring-based reclaim as Claude (see [The installed config shape](#the-installed-config-shape)); the legacy `[hooks.rimz]` table is ignored by Codex and exists only as uninstall cleanup.

Codex 0.134 routes thread-spawned subagents through `SubagentStart`/`SubagentStop` instead of the root `SessionStart`/`Stop` lifecycle. Hooks fired inside a subagent carry a child `agent_id` and `agent_type`; Rimz keys those rows by the child `agent_id`, so a pending subagent permission request replaces the subagent row rather than duplicating the parent session. The payload's `session_id` is the parent root, captured as `parent_agent_id` so the sidebar nests the child under its parent (see [sidebar.md → Sub-agent lists](./sidebar.md#sub-agent-lists)).

### App-server enrichment

Codex has no statusline, so its rich `AgentContext` (the analogue of Claude's statusline feed) is read from the official Codex app-server (`codex app-server`, JSON-RPC 2.0 over stdio). The client (`agents/codex_app_server.rs`) speaks only **read-only, non-interfering** methods — `initialize`/`initialized`, then `account/rateLimits/read` (the 5h/7d rate-limit windows, mapped from Codex's positional `primary`/`secondary` by `windowDurationMins`) and `model/list` (the session model's display name + effort, matched against the model id the lifecycle observation reports). The Codex version comes from the `initialize` `userAgent`. It never calls `thread/resume`, `turn/start`, or any write — those would rejoin and own the user's live thread.

The trigger is out-of-band, never inline: on the turn-boundary lifecycle events (`SessionStart`, `UserPromptSubmit`, `Stop`) the Codex hook spawns `rimz codex refresh-context` **detached with null stdio**, so the hook returns before the app-server round-trip runs and adds no latency to the turn. That helper writes the same per-session `AgentContext` sidecar Claude uses; the sidebar's next wakeup folds it in. A short freshness throttle skips a refresh when the session's sidecar is only seconds old, so two close boundaries don't each spawn a server. `RIMZ_CODEX_BIN` overrides the `codex` binary (tests point it at a stub). Everything is best-effort: a missing `codex`, an API-key account with no rate-limit windows, or any protocol hiccup degrades to an omitted field or no sidecar — never a failed hook.

Daemon re-use: when a Codex app-server daemon is running — the per-user singleton `codex remote-control start` brings up, which Rimz auto-launches detached (not a pane) when `[remote_control] codex` is on (see [remote control auto-launch](../reference/configuration.md#remote-control-auto-launch)) — the client prefers its control socket via `codex app-server proxy --sock <path>`, re-using that daemon instead of cold-spawning a throwaway server. The socket is `$CODEX_HOME/app-server-control/app-server-control.sock` (`~/.codex/...`), overridable by `RIMZ_CODEX_APP_SERVER_SOCK` (empty forces cold-spawn). The proxy probe runs on a tight budget and a fresh `codex app-server` is always tried as the fallback, so enrichment never depends on the daemon. The JSON-RPC contract is identical over either transport.

The one detail the app-server does **not** expose read-only is token / context-window usage: as of the pinned Codex, usage rides only the live `thread/tokenUsage/updated` notification behind a subscribing `thread/resume`. So the context-window gauge (`context_pct` / `total_tokens`) stays sourced from the rollout transcript tail (above); `AgentContext.tokens` is left `None` for Codex.

---

## Implementation status

The contract above is implemented. The history below is kept so the rationale for each fix stays discoverable.

1. **`UserPromptSubmit` is wired** in both adapters, so an install reaches `running` and carries the prompt as its task. `Stop` was already wired.
2. **`Stop` maps to `success`/`failed`** via a shared `stop_status_from_payload` — clean completion is `success`, an explicit error signal is `failed`. `idle` is owned by `SessionStart`/`SubagentStop`, never a `Stop` outcome.
3. **Context budget is captured** from the Claude transcript tail on `SessionStart`/`UserPromptSubmit`/`Stop`.
4. **Agent visibility no longer requires a pid.** `RuntimeScope::Runtime` applies the owner-required filter to `Surface::Script` items only; agents and bridge asks are kept unless a known owner is known-dead, so a pid-less agent still renders — on its stamped live pane.
5. **The branch label is re-derived live** from each worktree group's path by the snapshot CLI (cached under the diff-stats TTL), so the header follows a `git checkout`; the pinned ledger branch is the fallback when no live worktree resolves.
6. **Pane binding is by the stamped pane id alone.** Each lifecycle event stamps `TMUX_PANE`/`ZELLIJ_PANE_ID`, and the snapshot binds a live pane to the one agent that stamped it — one pane, one row, by construction. The earlier command/launcher heuristics (the `node`/`bun`/`deno`/`python` loose match) and their planned pid-ancestry refinement are removed as unnecessary: a pane the agent never stamped is a process row, full stop.
7. **A `Stop` parked on background work stays `running`.** When the main thread spawns background tasks/agents and parks (`✻ Waiting for N background agents to finish`), Claude fires `Stop` while the work is still in flight and reawakens the thread when it reports back. The adapter reads the `Stop` payload's `background_tasks` (Claude Code v2.1.145+): any in-flight entry upgrades the clean stop to `running` and labels the row with the background work, so the sidebar no longer paints `✓` on a busy agent. An absent or all-terminal array (older builds, genuine turn end) keeps the prior `success`/`failed` behaviour.
