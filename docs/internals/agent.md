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
| `kind`                              | `claude` / `codex`                | `--source` on the hook                                            | identity      |
| `status`                            | 5-value rollup (below)            | derived from `event_name` (§ state machine)                       | activity      |
| `permission_posture`                | `default`/`auto`/`yolo`/`unknown` | `SessionStart` · `permission_mode`/`approval_policy`              | carry-forward |
| `task`                              | what it's working on              | `UserPromptSubmit` · `prompt`; `SubagentStart` · `agent_type`     | activity      |
| `model`                             | `Opus`, `GPT-5.5`                 | lifecycle · `model`                                               | carry-forward |
| `effort`                            | `xhigh`/`high`/…                  | lifecycle · `thinking_level`/`model_reasoning_effort`             | carry-forward |
| `context_pct`                       | context-window % gauge            | payload or transcript tail (§ enrichment)                         | carry-forward |
| `total_tokens`                      | cumulative tokens                 | payload or transcript tail                                        | carry-forward |
| `todo_done`/`todo_total`            | plan progress dots                | Claude `TodoWrite` · `tool_input.todos`; Codex none today         | carry-forward |
| `agent_pid` / `agent_process_start` | liveness gate                     | `RIMZ_AGENT_PID=$PPID`, else `/proc` ancestor walk                | identity      |
| `runtime_owner`                     | owner-process identity            | built from `agent_pid` + start token                              | identity      |
| `worktree_path` / `worktree_branch` | grouping spine                    | live pane cwd → worktree (ledger value is detached-only fallback) | live-derived  |
| `pane`                              | jump target                       | bound live at snapshot from the pane list                         | live-derived  |
| `last_activity`                     | age + ranking key                 | `event.timestamp` of the agent's own latest event                 | activity      |
| `last_seen`                         | carryover-merge tiebreak          | `event.timestamp`                                                 | activity      |

The catalog turns on one distinction: **identity vs. live-derived**. `worktree_*` and `pane` are *live* facts — the pane knows its current cwd every tick — so they are derived at snapshot time, not pinned at session start. Pinning them is the branch-tracking bug (§ Liveness and presence).

### The state machine

The five-value status set, in ranking order (most attention-hungry first), per [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape):

| Status    | Glyph | Meaning                     | Raises attention |
| --------- | ----- | --------------------------- | ---------------- |
| `waiting` | `◆`   | blocked on a human decision | yes              |
| `failed`  | `✗`   | the last turn errored       | yes              |
| `running` | `◐`   | actively working a task     | no               |
| `idle`    | `○`   | wired in, nothing in flight | no               |
| `success` | `✓`   | last turn completed cleanly | no               |

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

The agent owns status and posture; Rimz observes and renders. `yolo` is observed from the agent's own bypass flag (`claude --dangerously-skip-permissions`, `codex --ask-for-approval never`). Workflow words such as `plan` and `interactive` fold into `default` because they are posture-neutral. The vocabulary is defined once in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape).

### Instance identity and age

An agent row belongs to **one running instance**. The key is `(kind, agent_id)` with `agent_id` the agent's session id, so two concurrent agents of the same kind never share a row and `last_activity` is always the agent's *own* latest event — never inherited from a previous instance of the same kind.

When a payload carries no session id, the adapter keys on the captured `runtime_owner` (pid + start token) rather than a shared anonymous bucket, so two unidentified instances never merge; a truly unkeyable event is dropped rather than collapsed — better no row than a row that lies about its age.

### Enrichment is display-only

`task`, `context_pct`, `total_tokens`, and the todo counts are **enrichment**: display-only, redactable, and they never drive routing, ranking, or a decision (the no-transcript-correctness rule). The reduced agent state keeps missing context as "the agent didn't report it"; the sidebar row projects that missing value to a visible 0% baseline so every observed agent has a context bar.

Context budget is the one field no agent puts directly in its hook JSON — usage lives in the transcript. Capture reads the **transcript tail** only after the agent payload supplies a `session_id`, on the low-frequency events Rimz already fires (`SessionStart`, `UserPromptSubmit`, `Stop`), takes the most recent assistant usage record, and scales it against the model's context window, so the gauge upgrades without a per-tool hook. These are bare token counts (metadata); `payload_mode` gates the *content* of telemetry events, never these gauges or the *state transition* they ride on.

## Liveness and presence

Presence comes from the live pane list, not from a session-exit hook (see [sidebar.md → Presence model](./sidebar.md#presence-model)) — an agent whose pane reverts to a shell or closes is gone with no event required. Precedence is fixed:

- **Foreground command is the primary, cross-backend signal.** A TUI agent holds its pane's foreground for its whole life; a pane that drops back to `zsh` drops its agent overlay; a closed pane is absent next tick.
- **The captured pid is a *refining gate*, never a requirement.** On a lifecycle event Rimz records the agent's pid best-effort — `RIMZ_AGENT_PID=$PPID` names the spawning agent, falling back to a `/proc` ancestor walk. On Linux it also records the process-start time to defeat pid reuse. A *known-dead* pid suppresses a stale overlay; an *unknown* pid abstains and lets the foreground signal carry liveness alone. Liveness suppresses; it never gates an agent in.

There is no `offline` status — a dead agent is a reverted shell row or no row at all, never a retracted ledger fact.

Two consequences this contract enforces (status in [Implementation status](#implementation-status)):

- **A stale overlay never paints a non-agent pane.** After an agent exits and `git log` runs in the same pane, the foreground is `git` — a process row, never the agent that just left. An overlay attaches to a non-shell pane only when its foreground maps to the agent kind or a known launcher (`node`, `bun`, `deno`, `python`…); the planned refinement also requires, when both pids are known, that the agent pid is an ancestor of the pane pid. The loose match exists for `node`-wrapped Codex, not for arbitrary commands.
- **Worktree and branch track the live pane.** Branch and worktree are resolved from the pane's current cwd at snapshot time (the same place diff stats are read), so they follow `git checkout` and a pane `cd` into another worktree. The ledger's pinned `worktree_*` is a fallback only for a detached agent with no live pane.

Pane binding and jump are the snapshot's job, documented in [sidebar.md → Jump](./sidebar.md#jump--the-row-is-the-link): the hook stamps the multiplexer's own per-pane env var (`TMUX_PANE` / `ZELLIJ_PANE_ID`) on every lifecycle event — the mux's ground-truth assignment, not an agent self-claim — so two same-kind agents in one worktree bind to their distinct panes; binding is otherwise live, exact match before loose, and every jump reconciles pane id *and* `pane_process_start` so a reused id never focuses a stranger.

## Hook install is an explicit, visible step

Hook install is a security surface. `rimz start` detects installed, supported agents each run, previews the additive per-user config change, installs missing hooks when approved, and continues without installing when the user skips or declines. `rimz hooks install <agent>` is the manual entry point. An agent run before hooks are installed fires no hook and registers nothing. `hooks_installed()` makes that state observable: `rimz doctor` reports it per agent, and the sidebar's first-run hint points at `rimz hooks install` until an agent is wired (see [sidebar.md → Empty-room hint](./sidebar.md#empty-room-hint)).

### Default vs. telemetry install

The default install wires every event the **state machine** needs; telemetry adds high-frequency, content-heavy hooks for audit depth.

```sh
rimz hooks install claude                  # lifecycle + feed: drives the full state machine
rimz hooks install claude --telemetry      # add per-tool hooks for audit depth
```

The split is **state signal vs. payload depth**, not "some transitions are optional". `UserPromptSubmit` and `Stop` are state signal — without them an agent never enters `running` and never carries a task — so they are default. `PostToolUse` and broad `PreToolUse` fire on every tool call and carry tool inputs, prompts, file paths, and outputs; they are telemetry, useful for `rimz feed list --audit` depth. Gate telemetry payloads against `[privacy] payload_mode`:

- `payload_mode = "metadata"` — strips inputs, prompts, args, errors. Smallest footprint.
- `payload_mode = "redacted"` — keeps bounded payloads with built-in redaction. Default.
- `payload_mode = "full"` — keeps hook payloads as delivered. `rimz doctor` warns.

Privacy gates the *content* of an event, never whether a state transition is observed.

## Adding an agent

OpenCode, Pi, Cursor, Gemini, Copilot, Amp, Rovo, Hermes, Factory, Qoder, and similar agents land through `AgentIntegration` once their hook surfaces and decision outputs are verified. The work is a new appendix below — the native-event → unified-interface mapping — plus the trait impl. Nothing else changes.

Adding an agent requires tests for: install/uninstall, lifecycle mapping (native event → observation → state), feed classification, neutral stdout, decision stdout, PID attribution, and version drift behaviour. Pinned hook stdout shapes live as inline `insta::assert_*_snapshot!(... @"...")` goldens inside each adapter module — see [`claude.rs`](../../crates/rimz/src/agents/claude.rs) and [`codex.rs`](../../crates/rimz/src/agents/codex.rs).

---

## Appendix — Claude Code

The mapping from Claude's native protocol onto the unified interface. The appendix says only *which native events are wired* and *how each maps* — the behaviour they drive is the state machine above.

Native event → unified mapping:

| Native event                  | Install   | Channel       | `observe_lifecycle` → status        | Normalized fields                           |
| ----------------------------- | --------- | ------------- | ----------------------------------- | ------------------------------------------- |
| `SessionStart`                | default   | lifecycle     | `idle`                              | posture, model, context/tokens (transcript) |
| `UserPromptSubmit`            | default   | lifecycle     | `running`                           | `task` = prompt; refresh context/tokens     |
| `Stop`                        | default   | lifecycle     | `success` (error → `failed`; in-flight `background_tasks` → `running`) | `task` = background work else clear; refresh context/tokens |
| `SessionEnd`                  | default   | lifecycle     | removed                             | —                                           |
| `Notification`                | default   | lifecycle     | none (silent)                       | —                                           |
| `PermissionRequest`           | default   | blocking-feed | `waiting`                           | —                                           |
| `PreToolUse: ExitPlanMode`    | default   | blocking-feed | `waiting`                           | plan approval                               |
| `PreToolUse: AskUserQuestion` | default   | blocking-feed | `waiting`                           | user question                               |
| `PostToolUse`                 | telemetry | lifecycle     | none                                | `TodoWrite` todos; context/tokens           |
| `PreToolUse` (broad)          | telemetry | lifecycle     | none                                | audit depth                                 |

Decision shapes — Claude requires `hookSpecificOutput`:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
```

```json
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {} } }
```

`ExitPlanMode` and `AskUserQuestion` require `updatedInput`. The Claude adapter sets `hook_cap = 120s` (upstream cap ~125s; Rimz leaves a 5s margin so the bridge times out before the agent kills the hook). The exact value is `CLAUDE_HOOK_CAP` in [`claude.rs`](../../crates/rimz/src/agents/claude.rs). Install merges non-destructively into `~/.claude/settings.json` under per-matcher `_rimz_managed` markers; blocking events are marked `_rimz_sync = true`.

## Appendix — Codex

| Native event                                          | Install   | Channel       | `observe_lifecycle` → status | Normalized fields                                |
| ----------------------------------------------------- | --------- | ------------- | ---------------------------- | ------------------------------------------------ |
| `SessionStart`                                        | default   | lifecycle     | `idle`                       | posture, model, effort                           |
| `UserPromptSubmit`                                    | default   | lifecycle     | `running`                    | `task` = prompt                                  |
| `SubagentStart`                                       | default   | lifecycle     | `running`                    | keyed by child `agent_id`; `task` = `agent_type` |
| `SubagentStop`                                        | default   | lifecycle     | `idle`                       | child row; clear task                            |
| `Stop`                                                | default   | lifecycle     | `success` (error → `failed`) | clear task                                       |
| `PermissionRequest`                                   | default   | blocking-feed | `waiting`                    | —                                                |
| `PreToolUse`/`PostToolUse` (broad)                    | telemetry | lifecycle     | none                         | audit depth                                      |

Decision shape — Codex permission hooks emit only `hookSpecificOutput.decision`:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow" } } }
```

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "deny", "message": "Blocked by repository policy." } } }
```

Never emit `updatedInput`, `updatedPermissions`, or `interrupt` for Codex permission hooks — those fields belong to other Codex hook types and corrupt the permission decision. Codex's hook cap is shorter than Claude's (`CODEX_HOOK_CAP`); chain budgets must account for it. Install writes inline `[[hooks.Event]]` tables in `~/.codex/config.toml`; the legacy `[hooks.rimz]` table is ignored by Codex and exists only as uninstall cleanup.

Codex 0.134 routes thread-spawned subagents through `SubagentStart`/`SubagentStop` instead of the root `SessionStart`/`Stop` lifecycle. Hooks fired inside a subagent carry a child `agent_id` and `agent_type`; Rimz keys those rows by the child `agent_id`, so a pending subagent permission request replaces the subagent row rather than duplicating the parent session.

---

## Implementation status

The contract above is implemented. The history below is kept so the rationale for each fix stays discoverable.

1. **`UserPromptSubmit` is default-install** in both adapters (it was telemetry-only), so a default install reaches `running` and carries the prompt as its task. `Stop` was already default.
2. **`Stop` maps to `success`/`failed`** via a shared `stop_status_from_payload` — clean completion is `success`, an explicit error signal is `failed`. `idle` is owned by `SessionStart`/`SubagentStop`, never a `Stop` outcome.
3. **Context budget is captured** from the Claude transcript tail on `SessionStart`/`UserPromptSubmit`/`Stop`.
4. **Agent visibility no longer requires a pid.** `RuntimeScope::Runtime` applies the owner-required filter to `Surface::Script` items only; agents and bridge asks are kept unless a known owner is known-dead, so a pid-less agent is carried by pane corroboration instead of vanishing.
5. **The branch label is re-derived live** from each worktree group's path by the snapshot CLI (cached under the diff-stats TTL), so the header follows a `git checkout`; the pinned ledger branch is the fallback when no live worktree resolves.
6. **The loose pane match requires a known launcher** (`node`, `bun`, `deno`, `python`), so a `git`/`vim` pane never hosts a stale agent overlay. *Remaining:* the pid-ancestry refinement (agent pid an ancestor of the pane pid when both are known) needs a `/proc` walk in the snapshot CLI and is not yet wired.
7. **A `Stop` parked on background work stays `running`.** When the main thread spawns background tasks/agents and parks (`✻ Waiting for N background agents to finish`), Claude fires `Stop` while the work is still in flight and reawakens the thread when it reports back. The adapter reads the `Stop` payload's `background_tasks` (Claude Code v2.1.145+): any in-flight entry upgrades the clean stop to `running` and labels the row with the background work, so the sidebar no longer paints `✓` on a busy agent. An absent or all-terminal array (older builds, genuine turn end) keeps the prior `success`/`failed` behaviour.
