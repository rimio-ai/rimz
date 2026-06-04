# Agent hooks

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

A coding agent reports to Rimz through hooks. This doc owns the agent boundary end to end: the one trait every agent speaks, the two channels a hook can take, how install wires it, and the per-provider mapping that translates a native protocol onto Rimz's internal types.

It is the **single home for the native-to-internal mapping**: which native events are wired, which channel each takes, and how each folds onto Rimz's internal types ([`AgentIntegration`](../../crates/rimz/src/agents/mod.rs), [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs), the lifecycle/blocking-feed channels). The raw upstream protocol — the full event catalog, the stdin payload schemas, and the verbatim decision JSON — lives in the per-provider reference: [adapter/claude-reference.md](./adapter/claude-reference.md) and [adapter/codex-reference.md](./adapter/codex-reference.md). The seam to the rest of the system is the observation: this doc *produces* it; [agent.md](./agent.md) folds it into the agent rollup; [sidebar.md](./sidebar.md) paints it.

Agents are *sources*, not a privileged path. Anything a hook does, a script can do through the same CLI — a hook is just an adapter that translates a native protocol onto `rimz event`/`rimz feed`.

## The seam — `AgentIntegration`

Every agent — Claude, Codex, and every future one — speaks to Rimz through one trait, [`AgentIntegration`](../../crates/rimz/src/agents/mod.rs). Adding an agent is implementing the trait; nothing downstream of it is agent-specific. The trait is the single place a native protocol diverges and the single place it is normalized. Its methods, by role (signatures live in the trait):

- **`classify_hook`** sorts a native event into one of the two channels below (or `Unknown`, dropped) and, for a blocking event, names the [`FeedKind`](../../crates/rimz/src/feed.rs).
- **`observe_lifecycle`** is the normalizer: it maps a native lifecycle event onto one [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs) — the unified event shape every downstream reducer reads. `None` means "no state transition here", so high-frequency events stay silent.
- **`render_decision`** / **`render_neutral`** emit the agent-native decision JSON when a resolver answers, and the neutral no-op when no one does.
- **`hook_cap`** is how long a blocking hook may park on the bridge before falling back to neutral — set from the upstream's published deadline (default [`DEFAULT_HOOK_CAP`](../../crates/rimz/src/agents/mod.rs), 300s).
- **`observe_context`** normalizes a rich out-of-band payload into [`AgentContext`](../../crates/rimz/src/agents/mod.rs); `ends_session` / `moves_on` mark the events that expire a session's pending asks.
- **`install_hooks`** / **`preview_hook_install`** / **`uninstall_hooks`** / **`hooks_installed`** / **`supports_hook_install`** own the per-user config write and report it.

Two invariants hold the seam shut:

- **Adapters never touch the ledger.** The adapter is a pure mapper. The [`rimz hooks feed`](../../crates/rimz/src/cli/hooks.rs) CLI owns every ledger write and all bridge I/O; it calls the adapter for classification and rendering only.
- **Nothing downstream reads a native payload.** The adapter emits exactly two things the rest of Rimz consumes — an `AgentLifecycleObservation` and a decision `Value`. A native field reached for outside an adapter is a mapping that belongs *in* the adapter.

## Two hook channels

`classify_hook` sorts every native event into one of two wired channels. The distinction is whether the hook can hold the agent open while Rimz waits for an answer.

**Lifecycle — fast, non-blocking.** Drives agent status, the turn phase, task, enrichment, and `rimz feed list --audit` depth. Each flows through `observe_lifecycle`; an event that carries no transition returns `None` and records nothing.

**Blocking-feed — holds the agent open.** A permission request, plan approval, or user question. It becomes a [`FeedItem`](../../crates/rimz/src/feed.rs), not an observation, and engages the three operating paths in [ledger.md](./ledger.md#the-three-paths-at-a-glance): bind a per-request socket and wait for a resolver (`bridge`), or write the item and return neutral so the agent's own UI asks (`native_ui`).

Blocking decision hooks must be **sync**. Installing one as async is a hard error — the agent would ignore the decision printed on stdout — and the installer rejects it.

### Hook stdout is the decision channel

This is the canonical statement of the rule the rest of the docs link to. A hook's stdout carries exactly one thing: the agent-native decision JSON, printed only when a resolver answers on the bridge. The neutral path prints nothing and exits 0 — the agent's own UI takes over. It follows that:

- **Logs never go to stdout.** They go to stderr or Rimz state logs (the `print_stdout` lint gates this — see [rust-conventions.md](../contributing/rust-conventions.md)).
- **Hook helper children get fresh, fully-piped stdio — never inherited.** A wrapped statusline command's stderr, a notification helper's chatter, must never leak onto the decision channel. A CI grep rejects `Stdio::inherit` in hook paths.
- **Every neutral and decision shape is golden-tested**, including the neutral no-op (see [Adding an agent](#adding-an-agent)).

## From native event to internals

A lifecycle hook fires → `classify_hook` returns `Lifecycle` → `observe_lifecycle` maps the payload onto an `AgentLifecycleObservation` → the CLI records it as an `agent.lifecycle` event. The observation is the contract boundary; from here [agent.md](./agent.md) owns the rollup, the state machine, and liveness. A new agent that emits well-formed observations gets all of that for free.

A blocking hook fires → `classify_hook` returns `BlockingFeed` with a `FeedKind` → the CLI writes a feed item and runs the three operating paths. On a resolver answer it calls `render_decision` and prints the JSON; on timeout (the `hook_cap`) or with no fresh resolver it calls `render_neutral` and the agent's UI takes over. The per-request socket, CAS, nonce, and late-answer rules live in [ledger.md](./ledger.md).

## Hook install — the visible security step

Installing hooks edits the agent's own config, so it is a security surface, never silent. `rimz start` detects installed, supported agents each run, previews the additive per-user change, installs on approval, and continues if the user skips. `rimz hooks install <agent>` / `uninstall <agent>` are the manual entry points. `hooks_installed()` makes the state observable: `rimz doctor` reports it per agent and the sidebar's first-run hint points at install until an agent is wired. An agent run before its hooks are installed fires nothing and is invisible — never silently broken.

**What install wires.** Every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment and audit depth current. The single source of truth for the wired set is each adapter's `INSTALLED_EVENTS` constant — not restated here. Per-tool payload *content* is gated by `[privacy] payload_mode` (`metadata` / `redacted` / `full`; see [configuration.md](../reference/configuration.md#privacy)); the gate strips content, never whether a transition is observed.

**The installed config shape.** Neither agent has a wildcard event key, so install writes one block per wired event. Inside that constraint it stays minimal:

- **One command for every event** — `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`, with no `--event`. The helper reads the event from the payload's `hook_event_name`; `--event` survives only as a manual debugging override.
- **Idempotent, self-healing reclaim.** Install reclaims every rimz-owned entry by the stable command substring `rimz hooks feed --source <agent>` — marked or not, with or without a legacy `--event` — then rewrites the canonical set, so duplicate or stale blocks never accumulate. User-authored hooks (no rimz command) are untouched.

**Trust.** Every hook command enters the executable-surface hash, so a tampered hook config demotes project trust to stale (see [trust.md](./trust.md)).

## Adding an agent

OpenCode, Pi, Cursor, Gemini, Copilot, and similar agents land through `AgentIntegration` once their hook surface and decision outputs are verified. The work is a new appendix below — the native-event → internal mapping — plus the trait impl. Nothing else changes.

The mapping has four jobs: route each native event to a channel; map lifecycle events to observations; render the agent's *own* decision shape (never reuse another agent's JSON — see the divergence below); and set `hook_cap` from the upstream's published deadline, leaving margin so the bridge times out before the agent kills the hook.

Required tests, per [testing.md](../contributing/testing.md): install/uninstall, lifecycle mapping (native event → observation → state), feed classification, neutral silence, decision stdout (allow / deny / modified-input where supported), malformed-payload handling, PID attribution, and version drift. Pinned stdout shapes live as inline `insta::assert_*_snapshot!(... @"...")` goldens inside each adapter module. The adapter-authoring contract is in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md).

> **Decision shapes diverge — never share one.** Claude and Codex both wrap a `PermissionRequest` answer in `hookSpecificOutput.decision`, but Codex rejects fields Claude requires (`updatedInput`, `updatedPermissions`, `interrupt`). Each adapter renders its own shape; copying one agent's JSON to another corrupts the decision.

---

## Appendix — Claude Code

Native event → internal mapping. The appendix says *which native events are wired* and *how each maps*; the behaviour they drive is the state machine in [agent.md](./agent.md), and the upstream events, payloads, and decision schema are in [adapter/claude-reference.md](./adapter/claude-reference.md).

| Native event                  | Channel       | `observe_lifecycle` → [`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs)                                          |
| ----------------------------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `SessionStart`                | lifecycle     | `Registered` - model, context window/tokens (transcript)                                                                     |
| `UserPromptSubmit`            | lifecycle     | `TurnStarted` - sanitized `task`/`prompt`; refresh context/tokens                                                            |
| `SubagentStart`               | lifecycle     | `SubagentStarted` - keyed by child `agent_id`; `parent_agent_id` = `session_id`; `task` = `subagent_type`/`description`      |
| `SubagentStop`                | lifecycle     | `SubagentStopped` - child row; keeps the type label and parent link                                                          |
| `Stop`                        | lifecycle     | `TurnEnded { errored, parked_on_background }` - refresh context/tokens; the row paints a `⋯ bg` marker when parked           |
| `SessionEnd`                  | lifecycle     | `Ended` → removed (`ends_session`)                                                                                           |
| `Notification`                | lifecycle     | none (silent)                                                                                                                |
| `PreToolUse` (broad)          | lifecycle     | none - audit depth                                                                                                           |
| `PostToolUse`                 | lifecycle     | `ToolUsed { mutates: true, edits }` for a mutating tool (else none) - `edits` for a file-writing tool; `TodoWrite` todos; context/tokens |
| `PreCompact`                  | lifecycle     | `Compacting` - stamps the head, keeps the prior status (see [agent.md](./agent.md#the-state-machine))                        |
| `PermissionRequest`           | blocking-feed | `waiting` - sync                                                                                                             |
| `PreToolUse: ExitPlanMode`    | blocking-feed | `waiting` - plan approval                                                                                                    |
| `PreToolUse: AskUserQuestion` | blocking-feed | `waiting` - user question                                                                                                    |

**Classification.** `ExitPlanMode` and `AskUserQuestion` ride the broad `PreToolUse` hook and self-classify from `tool_name`, so they need no dedicated matcher (Claude runs every matching matcher group, and the broad entry already covers them). A `Stop` carries the raw `errored` bit (`stop_payload_errored`) and whether in-flight `background_tasks` (Claude Code v2.1.145+) remain; the [`step`](./agent.md#the-state-machine) table turns those into the final status (a clean parked stop stays `running`, an error always wins). A turn Claude aborts on a provider API error fires **no `Stop` at all** (upstream's `StopFailure` event would carry it; unwired today): the rollup keeps `running`, and the dead turn is recovered display-side from the transcript's death certificate ([transcript.md → Turn-death marker](./transcript.md#appendix--claude-code), [adapter/claude-reference.md](./adapter/claude-reference.md#transcript-death-certificate)). Only a *mutating* `PostToolUse` (`tool_mutates`) records a signal — it is proof of real work; read-only tools stay silent so the lifecycle channel isn't flooded. The `edits` bit marks the file-writing subset (`tool_edits_files`), which ends the turn's thinking head ([agent.md → Thinking](./agent.md#thinking-is-the-turns-opening-phase)).

**Decision shapes.** Claude wraps a permission answer in `hookSpecificOutput.decision`; `ExitPlanMode` / `AskUserQuestion` answer on the `PreToolUse` event and **require** `updatedInput` (a missing field is a hard render error). The neutral path is empty stdout. The verbatim shapes are in [adapter/claude-reference.md](./adapter/claude-reference.md#hooks-rimz-wires); exact bytes are the inline goldens in [`claude/mod.rs`](../../crates/rimz/src/agents/claude/mod.rs).

**Cap & install.** `hook_cap` is 120s (`CLAUDE_HOOK_CAP`; upstream ~125s, with a 5s margin so the bridge times out before Claude kills the hook). Install merges non-destructively into `~/.claude/settings.json` under per-matcher `_rimz_managed` markers; only `PermissionRequest` carries `_rimz_sync = true`, and an existing async marker on it is a hard install error.

**Subagents.** Claude routes Task-tool children through `SubagentStart`/`SubagentStop`, keyed by the child `agent_id` with the payload `session_id` captured as `parent_agent_id`, so the child gets its own row nested under its parent (see [agent.md](./agent.md#instance-identity-and-age) and [sidebar.md](./sidebar.md)). Identity is keyed, never guessed: the shared [`resolve_subagent_identity`](../../crates/rimz/src/agents/mod.rs) requires a child id distinct from the parent. A subagent event missing one is **quarantined** — it yields no observation (logged at `error!` under `rimz::agent::lifecycle`) rather than folding onto, and renaming, the parent's row. The child's type label is identity, not activity: the reducer carries it forward, so a `SubagentStop` that omits or blanks `agent_type` leaves an already-started child labeled rather than degrading it to a `subagent <id>` placeholder. A stop-only child with no type is ignored at reduction time; it lacks enough identity to create a sidebar child row.

**Rich context.** Install also manages the statusline: it points `statusLine` at `rimz statusline feed --source claude`, non-destructively wrapping any existing command. It wraps the per-child `subagentStatusLine` the same way (at `rimz statusline feed --source claude --subagent`), harvesting each subagent's description, token count, and start time into the expanded card. Both wraps are a visible security surface — the consent gate summarizes each and the install diff shows them in full. The statusline transport, its `AgentContext` mapping, and the wrap mechanics live in [transcript.md → Appendix Claude Code](./transcript.md#appendix--claude-code).

## Appendix — Codex

Native event → internal mapping; the upstream events, payloads, and decision schema are in [adapter/codex-reference.md](./adapter/codex-reference.md).

| Native event                         | Channel       | `observe_lifecycle` → [`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs) | Normalized fields                                |
| ------------------------------------ | ------------- | ----------------------------------- | ------------------------------------------------ |
| `SessionStart`                       | lifecycle     | `Registered`, or `Compacting` when `source = "compact"` | model, effort                  |
| `UserPromptSubmit`                   | lifecycle     | `TurnStarted`                       | sanitized `task`/`prompt`                        |
| `SubagentStart`                      | lifecycle     | `SubagentStarted`                   | keyed by child `agent_id`; `task` = `agent_type` |
| `SubagentStop`                       | lifecycle     | `SubagentStopped`                   | child row; keeps the type label                  |
| `Stop`                               | lifecycle     | `TurnEnded { errored, parked_on_background: false }` | clear task                      |
| `PermissionRequest`                  | blocking-feed | `waiting`                           | —                                                |
| `PostToolUse` (mutating)             | lifecycle     | `ToolUsed { mutates: true, edits }` | `edits` for `apply_patch`; read-only tools stay silent |
| `PreToolUse` (broad)                 | lifecycle     | none                                | audit depth                                      |

Codex shares the same keyed subagent identity as Claude (`resolve_subagent_identity`): a `SubagentStart`/`SubagentStop` with no distinct child id is quarantined, never folded onto the parent. Codex has no `SessionEnd` or `Notification` hook, so `ends_session` is never true — a Codex session leaves the rollup by liveness alone (see [agent.md](./agent.md#liveness-and-presence)). It has no background-task parking, so `parked_on_background` is always `false`.

**Decision shape.** Codex permission hooks emit only `hookSpecificOutput.decision` (`behavior` plus an optional `message`); never `updatedInput`, `updatedPermissions`, or `interrupt`, which belong to other Codex hook types and corrupt the decision. The neutral path is empty stdout. The verbatim shape and the full divergence note are in [adapter/codex-reference.md](./adapter/codex-reference.md#decision-and-output-schema); exact bytes are the inline goldens in [`codex/mod.rs`](../../crates/rimz/src/agents/codex/mod.rs).

**Cap & install.** `hook_cap` is 60s (`CODEX_HOOK_CAP`); chain budgets must account for the shorter ceiling. Install writes inline `[[hooks.Event]]` tables in `~/.codex/config.toml` with the same `--event`-free command and substring reclaim as Claude; the legacy `[hooks.rimz]` table is ignored by Codex and removed on uninstall.

**Subagents.** Codex 0.134 routes thread-spawned subagents through `SubagentStart`/`SubagentStop` (a child `agent_id`, the parent root as `session_id` → `parent_agent_id`), keyed by the child so a subagent permission request replaces the subagent row, not the parent.

**Compaction.** Codex has no dedicated pre-compaction hook; it re-fires `SessionStart` with `source = "compact"` once the context is condensed. That source flags `compacting` (the other sources — `startup`/`resume`/`clear` — do not), so the sidebar shows a brief compacting head. Claude instead fires a true `PreCompact` *before* compaction (see the Claude appendix), so its head leads the work rather than trailing it.

### Session registration and launch quirks

Codex registers its session lazily. A plain CLI launch fires no `SessionStart`; the first prompt fires `SessionStart` and `UserPromptSubmit` together, both carrying the session id. So a freshly launched Codex is an agent instance with no session id until its first turn, which is why the sidebar synthesizes an idle row for it ([agent.md → The instance lifecycle](./agent.md#the-instance-lifecycle)).

`/clear` currently fires **no** `SessionStart` (the wired `source = "clear"` never arrives), so Rimz cannot yet detect a cleared session as a fresh instance — the prior session's row persists until the next bound turn. This is a known upstream gap; Rimz waits for `SessionStart { source: "clear" }` and treats the miss as a documented limitation rather than working around it.

Under `codex remote-control start` the hooks are daemon-routed: they fire from the shared per-user app-server daemon, so `pane_id` is null (the session is unstamped) and `RIMZ_AGENT_PID` is the daemon pid. Binding an unstamped in-pane session is the cwd fallback in [sidebar.md → Presence model](./sidebar.md#presence-model); the fallback reconciles against the in-pane `codex` CLI's `/proc` start, so a relaunched `codex` in a reused cwd starts fresh rather than adopting the prior session's stats. A session with no local pane is a *remote* agent, not rendered yet (same section).

### Context enrichment

Codex has no statusline: its `AgentContext` is read out of band from `codex app-server`, and its context gauge from the rollout transcript tail. The read-only client, the detached refresh trigger, the broker → daemon → cold-spawn connection preference, and the one gap (usage rides only a live notification, so the gauge stays transcript-sourced) all live in [transcript.md → Appendix Codex](./transcript.md#appendix--codex).
