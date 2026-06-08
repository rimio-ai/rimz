# Agent hooks

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

A coding agent reports to Rimz through hooks. This doc owns the agent boundary end to end: the one trait every agent speaks, the two channels a hook can take, how install wires it, and the per-provider mapping that translates a native protocol onto Rimz's internal types.

It is the **single home for the native-to-internal mapping**: which native events are wired, which channel each takes, and how each folds onto Rimz's internal types ([`AgentAdapter`](../../crates/rimz/src/agents/mod.rs), [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs), the lifecycle/blocking-feed channels). The raw upstream protocol — the full event catalog, the stdin payload schemas, and the verbatim decision JSON — lives in the per-provider reference: [claude-reference.md](../externals/agent-adapter/claude-reference.md), [codex-reference.md](../externals/agent-adapter/codex-reference.md), and [pi-reference.md](../externals/agent-adapter/pi-reference.md) ([opencode-reference.md](../externals/agent-adapter/opencode-reference.md) mirrors OpenCode ahead of its adapter). The seam to the rest of the system is the observation: this doc *produces* it; [agent.md](./agent.md) folds it into the agent rollup; [sidebar.md](./sidebar.md) paints it.

Agents are *sources*, not a privileged path. Anything a hook does, a script can do through the same CLI — a hook is just an adapter that translates a native protocol onto `rimz event`/`rimz feed`.

## The seam — `AgentAdapter`

Every agent — Claude, Codex, Pi, and every future one — speaks to Rimz through one trait, [`AgentAdapter`](../../crates/rimz/src/agents/mod.rs), registered in [`registry::ADAPTERS`](../../crates/rimz/src/agents/registry.rs). Adding an agent is implementing the trait plus a static [`AgentDescriptor`](../../crates/rimz/src/agents/descriptor.rs) (identity, branding, capabilities, tool tables) and one registry line; nothing downstream of it is agent-specific. The trait is the single place a native protocol diverges and the single place it is normalized. Its methods, by role (signatures live in the trait):

- **`classify_hook`** sorts a native event into one of the two channels below (or `Unknown`, dropped) and, for a blocking event, names the [`FeedKind`](../../crates/rimz/src/feed.rs).
- **`observe_lifecycle`** is the normalizer: it maps a native lifecycle event onto one [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs) — the unified event shape every downstream reducer reads. `None` means "no state transition here", so high-frequency events stay silent.
- **`render_decision`** / **`render_neutral`** emit the agent-native decision JSON when a resolver answers, and the neutral no-op when no one does.
- **`hook_cap`** (a descriptor field) is how long a blocking hook may park on the bridge before falling back to neutral — set from the upstream's published deadline.
- **`observe_context`** normalizes a rich out-of-band payload into [`AgentContext`](../../crates/rimz/src/agents/mod.rs); **`local_context_refresh`** derives sidecar fields from local provider state during non-blocking progress hooks; `ends_session` / `moves_on` mark the events that expire a session's pending asks.
- **`install_hooks`** / **`preview_hook_install`** / **`uninstall_hooks`** / **`hooks_installed`** own the per-user config write and report it (gated by the descriptor's `hook_install` capability).

Two invariants hold the seam shut:

- **Adapters never touch the ledger.** The adapter is a pure mapper. The [`rimz hooks feed`](../../crates/rimz/src/cli/hooks.rs) CLI owns every ledger write and all bridge I/O; it calls the adapter for classification and rendering only.
- **Nothing downstream reads a native payload.** The adapter emits exactly two things the rest of Rimz consumes — an `AgentLifecycleObservation` and a decision `Value`. A native field reached for outside an adapter is a mapping that belongs *in* the adapter.

## Two hook channels

`classify_hook` sorts every native event into one of two wired channels. The distinction is whether the hook can hold the agent open while Rimz waits for an answer.

**Lifecycle — fast, non-blocking.** Drives agent status, the turn phase, task, enrichment, and `rimz feed list --audit` depth. Each flows through `observe_lifecycle`; an event that carries no transition returns `None` and records nothing.

**Blocking-feed — holds the agent open.** A permission request, plan approval, or user question. It becomes a [`FeedItem`](../../crates/rimz/src/feed.rs), not an observation, and engages the three operating paths in [ledger.md](./ledger.md#the-three-paths-at-a-glance): bind a per-request socket and wait for a resolver (`bridge`), or write the item and return neutral so the agent's own UI asks (`native_ui`). The `native_ui` hand-off requires a surface to hand to: an agent whose descriptor declares `native_ask_ui` off (pi) resolves the same ask neutrally with **no feed item** — there is nothing the item could route the human to, so pushing one would strand it waiting.

Blocking decision hooks must be **sync**. Installing one as async is a hard error — the agent would ignore the decision printed on stdout — and the installer rejects it.

### Hook stdout is the decision channel

This is the canonical statement of the rule the rest of the docs link to. A hook's stdout carries exactly one thing: the agent-native decision JSON, printed only when a resolver answers on the bridge. The neutral path prints nothing and exits 0 — the agent's own UI takes over. It follows that:

- **Logs never go to stdout.** They go to stderr or Rimz runtime state logs such as `binding.log.jsonl` (the `print_stdout` lint gates this — see [rust-conventions.md](../contributing/rust-conventions.md)).
- **Hook helper children get fresh, fully-piped stdio — never inherited.** A wrapped statusline command's stderr, a notification helper's chatter, must never leak onto the decision channel. A CI grep rejects `Stdio::inherit` in hook paths.
- **Every neutral and decision shape is golden-tested**, including the neutral no-op (see [Adding an agent](#adding-an-agent)).

### Hooks resolve the room they live in

A hook resolves its workspace as a **participant** ([`WorkspaceResolver::resolve_participant`](../../crates/rimz/src/workspace.rs)): the session's identity pin — `RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT`, stamped into the mux environment at birth ([multiplexers.md → The identity pin](./multiplexers.md#the-identity-pin)) — wins over re-deriving identity from cwd, so an agent working inside a nested repo in a directory room writes to the room its pane lives in, never to a ledger no sidebar reads. The pin is hash-verified (`workspace_id` must hash from the pinned root) and any mismatch falls through to the static ladder (git → marker → directory): a hook on the agent's critical path degrades on identity, never errors. Every participant surface resolves the same way — `rimz event`/`feed`, the statusline helpers, the pane verbs, the sidebar renderer — and a CI grep (`cargo xtask invariants`) keeps the create-mode resolver out of them; room-choosing commands (`rimz start`/`attach`, maintenance) resolve statically, so a deliberate per-repo room can still be created from inside a parent room.

A **daemon-routed** hook (Codex's, fired from the shared per-user app-server — see [Appendix Codex](#appendix--codex)) inherits its daemon's environment, not the pane's, so the env pin never reaches it. `rimz hooks feed` recovers the pin from the sibling agent process instead ([`WorkspaceResolver::resolve_participant_with_pin_recovery`](../../crates/rimz/src/workspace.rs)): the daemon spawns hooks with the session cwd, so the in-pane agent process sharing that cwd carries the pane's pin in `/proc/<pid>/environ`. Each candidate is verified like the env pin and adopted only when every candidate names one root; an empty or split scan — and any non-Linux host — degrades to the static ladder. The full order: `--root`, env pin, recovered sibling pin, static ladder.

Session-to-pane binding diagnostics use the `rimz::agent::binding` tracing target and the workspace runtime `binding.log.jsonl`: exhausted daemon focus recovery and non-start events creating unseen sessions warn to the log stream, while each recovery attempt appends its probes, candidates, reject reasons, and outcome to the JSONL file. Hook stdout stays reserved for the decision channel.

## From native event to internals

A lifecycle hook fires → `classify_hook` returns `Lifecycle` → `observe_lifecycle` maps the payload onto an `AgentLifecycleObservation` → the CLI records it as an `agent.lifecycle` event. The observation is the contract boundary; from here [agent.md](./agent.md) owns the rollup, the state machine, and liveness. A new agent that emits well-formed observations gets all of that for free.

A blocking hook fires → `classify_hook` returns `BlockingFeed` with a `FeedKind` → the CLI writes a feed item and runs the three operating paths. On a resolver answer it calls `render_decision` and prints the JSON; on timeout (the `hook_cap`) or with no fresh resolver it calls `render_neutral` and the agent's UI takes over (an agent with no native ask UI just proceeds — neutral is its allow). The per-request socket, CAS, nonce, and late-answer rules live in [ledger.md](./ledger.md).

## Hook install — the visible security step

Installing hooks edits the agent's own config, so it is a security surface, never silent. `rimz start` detects installed, supported agents each run, previews the additive per-user change in an inline consent gate, lets Space toggle individual agents, shows a real unified diff with `d`, installs the selected agents on approval, and continues if the user skips. `rimz hooks install <agent>` / `uninstall <agent>` are the manual entry points. `hooks_installed()` makes the state observable: `rimz doctor` reports it per agent and the sidebar's first-run hint points at install until an agent is wired. An agent run before its hooks are installed fires nothing and is invisible — never silently broken.

**What install wires.** Every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment and audit depth current. Codex uses those progress events to stat-gate the rollout tail and push local token/cost context without waiting for the app-server. The single source of truth for the wired set is each adapter's `INSTALLED_EVENTS`-style constant — not restated here. Per-tool payload *content* is gated by `[privacy] payload_mode` (`metadata` / `redacted` / `full`; see [configuration.md](../reference/configuration.md#privacy)); the gate strips content, never whether a transition is observed.

**The installed config shape.** Claude and Codex have no wildcard event key, so install writes one block per wired event into their config files; Pi instead owns one whole extension file (see [Appendix Pi](#appendix--pi)). Inside the config-merge shape it stays minimal:

- **One command for every event** — `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`, with no `--event`. The helper reads the event from the payload's `hook_event_name`; `--event` survives only as a manual debugging override.
- **Idempotent, self-healing reclaim.** Install reclaims every rimz-owned entry by the stable command substring `rimz hooks feed --source <agent>` — marked or not, with or without a legacy `--event` — then rewrites the canonical set, so duplicate or stale blocks never accumulate. User-authored hooks (no rimz command) are untouched.

**Trust.** Every hook command enters the executable-surface hash, so a tampered hook config demotes project trust to stale (see [trust.md](./trust.md)).

## Adding an agent

OpenCode (surface mirrored and live-verified in [opencode-reference.md](../externals/agent-adapter/opencode-reference.md)), Cursor, Gemini, Copilot, and similar agents land through `AgentAdapter` once their hook surface and decision outputs are verified. The work is one new directory under [`crates/rimz/src/agents/`](../../crates/rimz/src/agents/AGENTS.md) — the trait impl, its `AgentDescriptor`, typed payloads, and `spend.rs` — plus one line in `registry::ADAPTERS` and a new appendix below (the native-event → internal mapping). Nothing else changes: spending, doctor, install, branding, and classification all resolve through the registry. The Pi adapter is the worked example — its surface landed inside `agents/pi/`, and its one genuinely new divergence (no native ask UI) became a descriptor capability the shared sites consult, never a per-agent branch at a dispatch site.

The mapping has four jobs: route each native event to a channel; map lifecycle events to observations; render the agent's *own* decision shape (never reuse another agent's JSON — see the divergence below); and set `hook_cap` from the upstream's published deadline, leaving margin so the bridge times out before the agent kills the hook.

Required tests, per [testing.md](../contributing/testing.md): install/uninstall, lifecycle mapping (native event → observation → state), feed classification, neutral silence, decision stdout (allow / deny / modified-input where supported), malformed-payload handling, PID attribution, and version drift. Pinned stdout shapes live as inline `insta::assert_*_snapshot!(... @"...")` goldens inside each adapter's `tests` module. The adapter-authoring contract is in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md).

> **Decision shapes diverge — never share one.** Claude and Codex both wrap a `PermissionRequest` answer in `hookSpecificOutput.decision`, but Codex rejects fields Claude requires (`updatedInput`, `updatedPermissions`, `interrupt`). Each adapter renders its own shape; copying one agent's JSON to another corrupts the decision.

---

## Appendix — Claude Code

Native event → internal mapping. The appendix says *which native events are wired* and *how each maps*; the behaviour they drive is the state machine in [agent.md](./agent.md), and the upstream events, payloads, and decision schema are in [claude-reference.md](../externals/agent-adapter/claude-reference.md).

| Native event                  | Channel       | `observe_lifecycle` → [`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs)                                          |
| ----------------------------- | ------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `SessionStart`                | lifecycle     | `Registered` - model, context window/tokens (transcript)                                                                     |
| `UserPromptSubmit`            | lifecycle     | `TurnStarted` - sanitized `task`/`prompt`; refresh context/tokens                                                            |
| `SubagentStart`               | lifecycle     | `SubagentStarted` - keyed by child `agent_id`; `parent_agent_id` = `session_id`; `task` = `subagent_type`/`description`      |
| `SubagentStop`                | lifecycle     | `SubagentStopped` - child row; keeps the type label and parent link                                                          |
| `Stop`                        | lifecycle     | `TurnEnded { errored, parked_on_background }` - refresh context/tokens; the row paints a `⋯ bg` marker when parked           |
| `StopFailure`                 | lifecycle     | no lifecycle envelope - writes `AgentContext.turn_error`; rollup stays `running`                                             |
| `SessionEnd`                  | lifecycle     | `Ended` → removed (`ends_session`)                                                                                           |
| `Notification`                | lifecycle     | none (silent)                                                                                                                |
| `PreToolUse` (broad)          | lifecycle     | `ToolUsed { mutates: false, edits: false }` as proof-of-work only; persisted only when it reconciles a resting row to `running` |
| `PostToolUse`                 | lifecycle     | `ToolUsed { mutates: true, edits }` for a mutating tool (else none) - `edits` for a file-writing tool; `TodoWrite` todos; context/tokens |
| `PreCompact`                  | lifecycle     | `Compacting` - stamps the head, keeps the prior status (see [agent.md](./agent.md#the-state-machine))                        |
| `PostCompact`                 | lifecycle     | `CompactionEnded` with known trigger - clears the head; auto resumes `running`, manual rests to `idle`                       |
| `PermissionRequest`           | blocking-feed | `waiting` - sync                                                                                                             |
| `PreToolUse: ExitPlanMode`    | blocking-feed | `waiting` - plan approval                                                                                                    |
| `PreToolUse: AskUserQuestion` | blocking-feed | `waiting` - user question                                                                                                    |

**Classification.** `ExitPlanMode` and `AskUserQuestion` ride the broad `PreToolUse` hook and self-classify from `tool_name`, so they need no dedicated matcher (Claude runs every matching matcher group, and the broad entry already covers them). A non-blocking `PreToolUse` is proof-of-work only: it does not join `activity_events`, and hook ingestion persists it only when `step` reconciles a resting row back to `running`. A `Stop` carries the raw `errored` bit (`stop_payload_errored`) and whether in-flight `background_tasks` (Claude Code v2.1.145+) remain; the [`step`](./agent.md#the-state-machine) table turns those into the final status (a clean parked stop stays `running`, an error always wins). `StopFailure` is Claude's provider-error certificate: `rate_limit` and `overloaded` write a paused-class `AgentContext.turn_error`, other errors write a failed-class marker, and no `agent.lifecycle` envelope is appended so the rollup stays `running`. The transcript death certificate is the backstop for old Claude builds or sessions whose hooks were installed late ([transcript.md → Turn-death marker](./transcript.md#appendix--claude-code), [claude-reference.md](../externals/agent-adapter/claude-reference.md#transcript-death-certificate)). Only a *mutating* `PostToolUse` (`tool_mutates`) records a signal — it is proof of real work; read-only tools stay silent so the lifecycle channel isn't flooded. The `edits` bit marks the file-writing subset (`tool_edits_files`), which ends the turn's thinking head ([agent.md → Thinking](./agent.md#thinking-is-the-turns-opening-phase)).

**Decision shapes.** Claude wraps a permission answer in `hookSpecificOutput.decision`; `ExitPlanMode` / `AskUserQuestion` answer on the `PreToolUse` event and **require** `updatedInput` (a missing field is a hard render error). The neutral path is empty stdout. The verbatim shapes are in [claude-reference.md](../externals/agent-adapter/claude-reference.md#hooks-rimz-wires); exact bytes are the inline goldens in [`claude/mod.rs`](../../crates/rimz/src/agents/claude/mod.rs).

**Cap & install.** `hook_cap` is 120s (`CLAUDE_HOOK_CAP`; upstream ~125s, with a 5s margin so the bridge times out before Claude kills the hook). Install merges non-destructively into `~/.claude/settings.json` under per-matcher `_rimz_managed` markers; only `PermissionRequest` carries `_rimz_sync = true`, and an existing async marker on it is a hard install error.

**Subagents.** Claude routes Task-tool children through `SubagentStart`/`SubagentStop`, keyed by the child `agent_id` with the payload `session_id` captured as `parent_agent_id`, so the child gets its own row nested under its parent (see [agent.md](./agent.md#instance-identity-and-age) and [sidebar.md](./sidebar.md)). Identity is keyed, never guessed: the shared [`resolve_subagent_identity`](../../crates/rimz/src/agents/mod.rs) requires a child id distinct from the parent. A subagent event missing one is **quarantined** — it yields no observation (logged at `error!` under `rimz::agent::lifecycle`) rather than folding onto, and renaming, the parent's row. The child's type label is identity, not activity: the reducer carries it forward, so a `SubagentStop` that omits or blanks `agent_type` leaves an already-started child labeled rather than degrading it to a `subagent <id>` placeholder. A stop-only child with no type is ignored at reduction time; it lacks enough identity to create a sidebar child row.

**In-subagent attribution.** Claude stamps `agent_id` on *every* payload fired inside a subagent ([claude-reference.md](../externals/agent-adapter/claude-reference.md)), so attribution is total: only the `Subagent*` brackets fold to the child's rollup, and any other event carrying a distinct `agent_id` — a backgrounded child's `PreToolUse`/`PostToolUse`, an in-subagent `PreCompact`/`PostCompact` — is dropped at the adapter ([`resolve_root_identity`](../../crates/rimz/src/agents/mod.rs), logged at `debug!`). The lifecycle channel is bracket-grained for children; per-tool child activity rides the child-keyed activity heartbeat ([agent.md → Liveness](./agent.md#liveness-and-presence)). The drop is load-bearing for attention: folded onto the parent, a backgrounded child's tool events would advance the parent's `last_activity` past a pending `native_ui` ask and un-fold its `waiting` row while the parent is still blocked.

**Rich context.** Install also manages the statusline: it points `statusLine` at `rimz statusline feed --source claude`, non-destructively wrapping any existing command. It wraps the per-child `subagentStatusLine` the same way (at `rimz statusline feed --source claude --subagent`), harvesting each subagent's description, token count, and start time into the expanded card. Both wraps are a visible security surface — the consent gate summarizes each and the install diff shows them in full. The statusline transport, its `AgentContext` mapping, and the wrap mechanics live in [transcript.md → Appendix Claude Code](./transcript.md#appendix--claude-code).

## Appendix — Codex

Native event → internal mapping; the upstream events, payloads, and decision schema are in [codex-reference.md](../externals/agent-adapter/codex-reference.md).

| Native event                         | Channel       | `observe_lifecycle` → [`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs) | Normalized fields                                |
| ------------------------------------ | ------------- | ----------------------------------- | ------------------------------------------------ |
| `SessionStart`                       | lifecycle     | `Registered`; `source = "compact"` is a no-op           | model, effort                  |
| `UserPromptSubmit`                   | lifecycle     | `TurnStarted`                       | sanitized `task`/`prompt`                        |
| `SubagentStart`                      | lifecycle     | `SubagentStarted`                   | keyed by child `agent_id`; `task` = `agent_type` |
| `SubagentStop`                       | lifecycle     | `SubagentStopped`                   | child row; keeps the type label                  |
| `Stop`                               | lifecycle     | `TurnEnded { errored, parked_on_background: false }` | clear task                      |
| `PermissionRequest`                  | blocking-feed | `waiting`                           | —                                                |
| `PostToolUse` (mutating)             | lifecycle     | `ToolUsed { mutates: true, edits }` | `edits` for `apply_patch`; read-only tools stay silent |
| `PreToolUse` (broad)                 | lifecycle     | `ToolUsed { mutates: false, edits: false }` as proof-of-work only | persisted only when it reconciles a resting row to `running` |
| `PreCompact`                         | lifecycle     | `Compacting`                        | stamps the head                                  |
| `PostCompact`                        | lifecycle     | `CompactionEnded` with known trigger | clears the head; auto resumes `running`, manual rests to `idle` |

Codex shares the same keyed subagent identity as Claude (`resolve_subagent_identity`): a `SubagentStart`/`SubagentStop` with no distinct child id is quarantined, never folded onto the parent. The root arm shares Claude's drop rule too (`resolve_root_identity`): a non-`Subagent*` event carrying a distinct `agent_id` folds to nothing rather than keying a parentless phantom root — latent today, since Codex stamps `agent_id` only on `Subagent*`. Codex has no `SessionEnd` or `Notification` hook, so `ends_session` is never true — a Codex session leaves the rollup by liveness alone (see [agent.md](./agent.md#liveness-and-presence)). It has no background-task parking, so `parked_on_background` is always `false`.

**Decision shape.** Codex permission hooks emit only `hookSpecificOutput.decision` (`behavior` plus an optional `message`); never `updatedInput`, `updatedPermissions`, or `interrupt`, which belong to other Codex hook types and corrupt the decision. The neutral path is empty stdout. The verbatim shape and the full divergence note are in [codex-reference.md](../externals/agent-adapter/codex-reference.md#decision-and-output-schema); exact bytes are the inline goldens in [`codex/mod.rs`](../../crates/rimz/src/agents/codex/mod.rs).

**Cap & install.** `hook_cap` is 60s (`CODEX_HOOK_CAP`); chain budgets must account for the shorter ceiling. Install writes inline `[[hooks.Event]]` tables in `~/.codex/config.toml` with the same `--event`-free command and substring reclaim as Claude; the legacy `[hooks.rimz]` table is ignored by Codex and removed on uninstall. Codex gates installed hooks behind its own per-hash trust state and **silently skips** an untrusted hook ([codex-reference.md → Trust state](../externals/agent-adapter/codex-reference.md#trust-state)) — only the user can open the channel (`/hooks` inside Codex), so the adapter reports installed-but-untrusted events (`untrusted_installed_hooks`) and `rimz start`/`rimz doctor` surface the fix.

**Subagents.** Codex 0.134 routes thread-spawned subagents through `SubagentStart`/`SubagentStop` (a child `agent_id`, the parent root as `session_id` → `parent_agent_id`), keyed by the child so a subagent permission request replaces the subagent row, not the parent.

**Compaction.** Claude and Codex both use the compaction pair as the authority: `PreCompact` stamps the head, and `PostCompact` clears it with the trigger split described in [agent.md](./agent.md#the-state-machine). Codex can still re-fire `SessionStart` with `source = "compact"` alongside the pair; the adapter treats that legacy echo as a no-op so a late `SessionStart(compact)` cannot re-light the head for another window.

### Session registration and launch quirks

Codex registers its session lazily. A plain CLI launch fires no `SessionStart`; the first prompt fires `SessionStart` and `UserPromptSubmit` together, both carrying the session id. So a freshly launched Codex is an agent instance with no session id until its first turn, which is why the sidebar synthesizes an idle row for it ([agent.md → The instance lifecycle](./agent.md#the-instance-lifecycle)).

`/clear` currently fires **no** `SessionStart` (the wired `source = "clear"` never arrives), so Rimz cannot yet detect a cleared session as a fresh instance — the prior session's row persists until the next bound turn. This is a known upstream gap; Rimz waits for `SessionStart { source: "clear" }` and treats the miss as a documented limitation rather than working around it.

Codex hooks are **daemon-routed** (since 0.137 for a plain TUI launch, not just under `codex remote-control start`): they fire from the shared per-user app-server daemon with the session cwd as working directory and the **daemon's environment** — so `pane_id` is null (the session is unstamped), `RIMZ_AGENT_PID` is the daemon pid, and the env pin never arrives; `rimz hooks feed` recovers it from the in-pane `codex` process at the same cwd ([Hooks resolve the room they live in](#hooks-resolve-the-room-they-live-in)). Binding an unstamped in-pane session is the cwd fallback in [sidebar.md → Presence model](./sidebar.md#presence-model); the fallback reconciles against the in-pane `codex` CLI's `/proc` start, so a relaunched `codex` in a reused cwd starts fresh rather than adopting the prior session's stats. A session with no local pane is a *remote* agent, not rendered yet (same section).

### Context enrichment

Codex has no statusline: its live usage context is read from the rollout transcript tail, while app-server metadata is read out of band from `codex app-server`. `local_context_refresh` runs on `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop`, stat-gates the rollout, and merges tokens/cost into the sidecar before any detached helper runs. The read-only app-server client, the detached metadata refresh trigger, the broker → daemon → cold-spawn connection preference, and the one gap (usage rides only a live notification, so Rimz keeps usage rollout-sourced) all live in [transcript.md → Appendix Codex](./transcript.md#appendix--codex).

## Appendix — Pi

Native event → internal mapping; the upstream extension API, payloads, and session JSONL are in [pi-reference.md](../externals/agent-adapter/pi-reference.md). Pi's integration surface is in-process TypeScript extensions, so the adapter ships one — [`extension.ts`](../../crates/rimz/src/agents/pi/extension.ts), embedded at compile time — that forwards each event below to `rimz hooks feed --source pi` as a fire-and-forget child. The child direction inverts: Claude and Codex run Rimz as a hook child and read its stdout; pi's extension runs Rimz as *its* child. One event blocks: `tool_call`, pi's pre-tool gate, whose handler pi awaits — there the extension reads the child's stdout as the decision and applies it through the handler's return value. Every envelope also stamps the model, the thinking level as `effort`, and the context gauge (`context_pct` / `context_window` / `total_tokens`, rounded) from the in-process `ctx.getContextUsage()`, so a pi row's gauge is payload-first with no transcript tail read.

| Native event             | Channel   | `observe_lifecycle` → [`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs) | Normalized fields                          |
| ------------------------ | --------- | ------------------------------------------------ | ------------------------------------------ |
| `session_start`          | lifecycle | `Registered`                                     | worktree from `cwd`                        |
| `before_agent_start`     | lifecycle | `TurnStarted`                                    | sanitized `prompt` (labels the row)        |
| `agent_end`              | lifecycle | `TurnEnded { errored, parked_on_background: false }` | `model`, `total_tokens`                |
| `tool_execution_end` (mutating) | lifecycle | `ToolUsed { mutates: true, edits }`       | `edits` for `edit`/`write`; `bash` mutates only; read-only tools stay silent |
| `tool_call`              | **blocking-feed** (`Permission`) | —                         | `tool_name` (lowercase), `tool_input`; pi awaits the handler |
| `session_before_compact` | lifecycle | `Compacting`                                     | a leading signal, like Claude's `PreCompact` |
| `session_compact`        | lifecycle | `CompactionEnded` with unknown trigger            | clears the head and preserves the prior status/phase |
| `session_shutdown`       | lifecycle | `Ended` → removed (`ends_session`)               | fires on quit incl. Ctrl+C/SIGHUP/SIGTERM and on `/new`/`/resume` replacement |

Pi's vocabulary maps onto Rimz's turn cleanly: a pi *turn* is one LLM call, and its `agent_*` pair brackets one user prompt — pi's `agent_*` is what Rimz calls a turn. The `agent_end` error bit is in band: a failed or aborted LLM call still ends with an assistant message carrying `stopReason: "error" | "aborted"` plus `errorMessage` — an explicit death certificate at the turn boundary, with no transcript forensics needed (unlike Claude's recovered `StopFailure` gap). Pi's compaction lifecycle is a true bracket (`session_before_compact`/`session_compact`), but the extension event omits the manual/auto reason that headless JSON mode carries, so Rimz clears the head without changing the underlying lifecycle status. Identity is direct: the extension runs in-process in the pane, the session id exists from launch (no lazy-registration window), and there is no daemon or remote mode, so every pi session is standalone and stamped.

**The blocking gate, without a native prompt.** Pi intentionally ships no permission prompts, plan approvals, or questions of its own — tools run unasked. What it does expose is `tool_call`, an awaited pre-tool gate, and Rimz wires it as the blocking-feed channel so an **enrolled resolver** can allow or deny each tool. The boundary holds in both directions: with no fresh resolver (or a stale-heartbeat downgrade, or an exhausted chain) the hook answers neutral — empty stdout, the tool runs — and pushes **no feed item**, because `native_ask_ui` is declared off and a `native_ui` row would strand waiting on a prompt pi never draws. Gating is opt-in via a resolver, never Rimz posing questions pi would not have asked; un-enrolled, a pi install behaves exactly like pi alone. Subagents, todos, background tasks, and the rate-limit/plan surface stay declared off in the descriptor and the absences render deliberately. A single pi session can also switch provider accounts mid-session, so the provider dashboard keys the panel by the agent kind — *pi* — and aggregates whatever accounts pi used.

**Decision shape.** Pi's own `ToolCallEventResult`: deny is `{"block": true, "reason": …}` (the resolver's reason, falling back to the decision's, then `denied by resolver`), allow is `{}`. Pi mutates tool input only in-process (the handler edits `event.input`), so a modified-input resolution renders the plain allow. `render_neutral` prints nothing — empty stdout is the allow, the only safe default for an agent with no prompt to fall back to.

**Cap & install.** Pi imposes no handler deadline, so `hook_cap` (120s) is purely Rimz's bridge ceiling — matched to Claude's so a resolver chain budgets identically across agents. Install is whole-file ownership keyed on the first-line `_rimz_managed` marker: a marked file (Rimz wrote it, however edited since) is reclaimed verbatim on re-install; an unmarked file at the path is the user's own extension and install, preview, and uninstall all refuse to touch it. The managed file is removed whole on uninstall and hot-reloads via `/reload`; the spawned child needs `rimz` on `PATH` (or `RIMZ_BIN`), and the extension skips the `reason: "reload"` shutdown so a `/reload` never tombstones the session it is about to re-register.

**Resume.** `pi --session <session_id>` restores a rollup-recorded session (a partial UUID suffices); the extension re-fires `session_start` with `reason: "resume"`.

**Integration-blind modes.** `--no-extensions` runs with no events at all, and `-p` / `--mode json` run extensions without a UI — same posture as an agent run before `rimz hooks install`: invisible, never silently broken.
