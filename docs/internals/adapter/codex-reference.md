# Codex protocol reference

> The mapping onto Rimz's internal types lives beside this doc: [hooks.md](../hooks.md) maps hook events to lifecycle/feed channels, [transcript.md](../transcript.md) maps the rollout transcript and app-server onto `AgentContext`, [account.md](../account.md) maps the auth surface onto account and balance.

This is the single home for the **Codex upstream protocol surface** Rimz binds to — the hook events and their decision schema, the `notify` channel, the app-server JSON-RPC API, the rollout transcript, and the auth file. It is a hand-maintained mirror of OpenAI's published docs and the open-source `codex-rs` types, kept for fast lookup and pinned to the source URLs below. The [`CodexAdapter`](../../../crates/rimz/src/agents/codex/mod.rs) adapter and the [`codex::app_server`](../../../crates/rimz/src/agents/codex/app_server.rs) client are the only code that reads this surface.

Coverage is **depth on what Rimz wires, breadth as an index**: the events, app-server methods, and rollout fields the code actually parses or emits are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists.

## Upstream sources

Re-fetch these pages — and, for the app-server, re-run the schema generators — to refresh this mirror.

| Surface | Source |
| --- | --- |
| Hooks reference (events, payloads, decision schema, trust) | <https://developers.openai.com/codex/hooks> |
| Hook executor (cwd/env semantics) | <https://github.com/openai/codex/blob/main/codex-rs/hooks/src/engine/command_runner.rs> |
| Config reference (`notify`, `[tui]` notifications) | <https://developers.openai.com/codex/config-reference> |
| Advanced config (`notify` payload) | <https://developers.openai.com/codex/config-advanced> |
| App-server API (protocol, methods, notifications) | <https://developers.openai.com/codex/app-server> |
| App-server README + schema generation | <https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md> |
| Rollout/session JSONL + `auth.json` shape | open-source `codex-rs` types — <https://github.com/openai/codex> |

The app-server protocol has no published version string; the canonical, version-exact schema is generated from the Codex binary itself:

```bash
codex app-server generate-ts --out DIR           # TypeScript bindings
codex app-server generate-json-schema --out DIR  # JSON Schema bundle
```

## Hooks

Codex hooks mirror Claude's shape: a command Codex runs at a lifecycle point, fed a JSON payload on **stdin**, returning a decision on **stdout**. They are wired in `~/.codex/config.toml` as `[[hooks.Event]]` tables. Rimz's [`CodexAdapter`](../../../crates/rimz/src/agents/codex/mod.rs) `INSTALLED_EVENTS` constant is the source of truth for the wired set; the native-event → Rimz status mapping is the [hooks.md Codex appendix](../hooks.md#appendix--codex).

**Execution.** A hook command runs with the **session cwd** as working directory and the **spawning process's environment** (`command_runner.rs`: no `env_clear`, per-handler overlays only). Since 0.137 a plain TUI launch routes hooks through the shared per-user app-server daemon, so the hook child's parent — and its environment — is the daemon's, not the pane's; the mux-stamped identity pin never arrives via env, and Rimz recovers it from the in-pane process instead ([hooks.md → Hooks resolve the room they live in](../hooks.md#hooks-resolve-the-room-they-live-in)).

### Config shape

```toml
[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = '/usr/bin/python3 "path/to/script.py"'
command_windows = 'py -3 C:\path\script.py'
timeout = 30
statusMessage = "Checking Bash command"
```

`matcher` is a regex over the tool name (or source, for `SessionStart`). Default `timeout` is **600** seconds.

### Trust state

Codex requires the user to review and trust each non-managed hook definition before it runs, records trust against the definition's hash, and **silently skips** a new or changed hook until it is trusted — `/hooks` inside Codex is the management UI (`--dangerously-bypass-hook-trust` bypasses for one invocation). Trust lands in the user config as `[hooks.state]` entries keyed `"<config-path>:<event_token>:<i>:<j>"`, the event token in lower_snake:

```toml
[hooks.state."/home/user/.codex/config.toml:permission_request:0:0"]
trusted_hash = "sha256:…"
```

A fresh `rimz hooks install` — or any change to the installed command — is therefore a wired-but-dead channel until the user trusts it inside Codex. Rimz detects the gap presence-only (`untrusted_hook_events_at` matches installed events against the state keys by token; the hash algorithm stays Codex's), and `rimz start`/`rimz doctor` surface the fix ([hooks.md → Appendix Codex](../hooks.md#appendix--codex)).

### Common input

Every hook receives:

```json
{
  "session_id": "string",
  "transcript_path": "string | null",
  "cwd": "string",
  "hook_event_name": "string",
  "model": "string",
  "permission_mode": "default | acceptEdits | plan | dontAsk | bypassPermissions",
  "turn_id": "string — turn-scoped events only"
}
```

Rimz parses around `permission_mode` without consuming it — the upstream still sends it; the agent model derives the turn phase from tool events instead.

### Events and per-event input

| Event | Fires | Event-specific input | Wired |
| --- | --- | --- | :---: |
| `SessionStart` | session starts / resumes / clears / compacts | `source` (`startup`\|`resume`\|`clear`\|`compact`) | ✓ |
| `UserPromptSubmit` | user submits a prompt | `turn_id`, `prompt` | ✓ |
| `SubagentStart` | a subagent launches | `turn_id`, `agent_id`, `agent_type`, `permission_mode` | ✓ |
| `PreToolUse` | before Bash / `apply_patch` / MCP tools | `turn_id`, `tool_name`, `tool_use_id`, `tool_input` | ✓ |
| `PermissionRequest` | approval needed (shell escalation, network) | `turn_id`, `tool_name`, `tool_input`, `tool_input.description?` | ✓ |
| `PostToolUse` | after tool output is produced | `turn_id`, `tool_name`, `tool_use_id`, `tool_input`, `tool_response` | ✓ |
| `SubagentStop` | a subagent stops | `turn_id`, `agent_id`, `agent_type`, `agent_transcript_path`, `stop_hook_active`, `last_assistant_message` | ✓ |
| `Stop` | turn completes | `turn_id`, `stop_hook_active`, `last_assistant_message` | ✓ |
| `PreCompact` | before conversation compaction | `turn_id`, `trigger` (`manual`\|`auto`) | |
| `PostCompact` | after compaction | `turn_id`, `trigger` | |

Codex has **no `SessionEnd` or `Notification` hook**. Compaction re-fires `SessionStart` with `source = "compact"` rather than a dedicated hook.

**Observed registration quirks** (upstream, pinned for refresh): `SessionStart` does not fire on a plain CLI launch — it rides the first `UserPromptSubmit` — and does not fire on `/clear` despite the documented `source = "clear"`. Rimz's handling is in the [hooks.md Codex appendix](../hooks.md#appendix--codex); re-verify against the hooks reference URL above on each refresh.

### Decision and output schema

`PermissionRequest` (the only shape Rimz renders):

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow|deny", "message": "string" } } }
```

> **Divergence — never reuse Claude's shape.** A Codex `PermissionRequest` decision carries only `decision.behavior` and `message`. Emitting `updatedInput`, `updatedPermissions`, or `interrupt` corrupts it — those belong to *other* Codex hook types (e.g. `PreToolUse`, below), not to a permission answer.

For reference, Codex's other decision shapes (Rimz does not currently emit these):

```json
// PreToolUse — allow (may carry updatedInput) / deny (carries permissionDecisionReason)
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": { "command": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "string" } }

// SessionStart / PreCompact / PostCompact / UserPromptSubmit / SubagentStop / Stop — common controls
{ "continue": true, "stopReason": "string", "systemMessage": "string", "suppressOutput": false, "hookSpecificOutput": { "additionalContext": "string" } }

// PostToolUse / Stop / SubagentStop — block
{ "decision": "block", "reason": "string" }
```

**Exit codes.** Exit `0` with JSON processes the output; exit `0` with no output continues; exit `2` is a blocking failure (stderr read as the reason/message). The neutral path Rimz takes is empty stdout, exit 0. Exact bytes are the inline goldens in [`codex/mod.rs`](../../../crates/rimz/src/agents/codex/mod.rs).

## `notify` channel

Independent of hooks, Codex can invoke an external program on supported events. Rimz uses **hooks, not `notify`** — this is recorded for reference. The `notify` key must live in the user-level `~/.codex/config.toml` (a project-local `notify` is ignored with a startup warning).

```toml
notify = ["python3", "/path/to/notify.py"]
```

The program receives a single JSON argument:

```json
{
  "type": "agent-turn-complete",
  "thread-id": "string",
  "turn-id": "string",
  "cwd": "string",
  "input-messages": ["string — user messages preceding the turn"],
  "last-assistant-message": "string"
}
```

Event types: `agent-turn-complete` (the primary, currently only, `notify` event) and `approval-requested` (referenced for TUI filtering). The in-terminal `[tui]` notifications are a separate channel:

```toml
[tui]
notifications = true            # bool, or an array of event types to restrict to
notification_method = "auto"    # auto | osc9 | bel
notification_condition = "unfocused"  # unfocused | always
```

## App-server API

Codex has no statusline, so Rimz reads its rich context out of band from the **app-server**: a bidirectional JSON-RPC 2.0 service (the `"jsonrpc":"2.0"` header is omitted on the wire), streamed as JSONL over stdio by default. Transports: `stdio://` (default), `ws://IP:PORT`, `unix://[PATH]`, or `off`. Start with `codex app-server` (or `--listen …`). A client must send one `initialize` request per connection, then an `initialized` notification, before any other method.

The protocol is organized around three primitives: an **Item** (atomic input/output unit with a `started` → optional `delta` → `completed` lifecycle), a **Turn** (the items from one unit of agent work), and a **Thread** (the durable session container).

### Methods Rimz uses

The [`codex::app_server`](../../../crates/rimz/src/agents/codex/app_server.rs) client speaks only **read-only, non-interfering** methods — it never calls `thread/resume`, `turn/start`, or any write, which would rejoin and own the user's live thread.

**`thread/loaded/list`** → the thread ids the app-server currently holds in memory; the daemon-mode liveness signal Rimz reaps ghost sessions against ([sidebar.md → Presence model](../sidebar.md#presence-model)).

```jsonc
// result — a flat list of loaded thread ids
{ "threadIds": ["string", …] }
```

The reaper queries the per-user daemon **specifically** (never a cold-spawn, whose empty set would mass-reap) and trusts only a recognized id-list shape: a response with no id field is treated as unknown, not zero, so a wire-shape drift keeps every session. The set is loaded-in-memory, not attached-pane, so it is a liveness improvement, not a perfect pane signal.

**`initialize`** → handshake; the response `userAgent` carries the Codex version.

```jsonc
// request
{ "method": "initialize", "params": { "clientInfo": { "name": "rimz", "version": "x.y.z" } } }
// then, as a notification:
{ "method": "initialized", "params": {} }
```

**`account/rateLimits/read`** → the 5h/7d balance windows and plan tier.

```jsonc
// result — RateLimitsResponse { rateLimits: RateLimitSnapshot }
{
  "rateLimits": {
    "primary":   { "usedPercent": 0-100, "resetsAt": <epoch s>, "windowDurationMins": 300 },   // ~5h
    "secondary": { "usedPercent": 0-100, "resetsAt": <epoch s>, "windowDurationMins": 10080 }, // ~7d
    "planType": "plus | pro | team | …"
  }
}
```

Fields are `camelCase` on the wire (`#[serde(rename_all = "camelCase")]`); `secondary` may be `null`, and a server-side change in window count or length renders gracefully off `windowDurationMins` rather than a hard-coded 5h/7d.

**`model/list`** (`{ "includeHidden": true }`) → the session model's display name. The payload also carries `defaultReasoningEffort`, but Rimz does not map it to row effort because it is a catalog default/recommendation, not the current session's configured value.

```jsonc
// result.data[] (RawModel)
{ "id": "string", "model": "string", "displayName": "string", "defaultReasoningEffort": "string?" }
```

**The token-usage gap.** The app-server does **not** expose token / context-window usage read-only — it rides only the live `thread/tokenUsage/updated` notification behind a subscribing `thread/resume`. So Codex's context gauge is sourced from the rollout transcript below, not the app-server.

### Method index (the rest)

A non-exhaustive map of the broader surface, for future wiring. Generate the exact, version-pinned schema with `codex app-server generate-json-schema`.

- **Thread**: `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/list`, `thread/archive`, `thread/name/set`, `thread/goal/{set,get,clear}`, `thread/compact/start`, `thread/rollback`, `thread/inject_items`.
- **Turn**: `turn/start`, `turn/steer`, `turn/interrupt`; `review/start`.
- **Account / auth**: `account/read`, `account/login/{start,cancel}`, `account/logout`, `account/rateLimits/read`, `account/sendAddCreditsNudgeEmail`.
- **Tools / exec / fs**: `command/exec` (+ `write`/`resize`/`terminate`), `process/{spawn,writeStdin,resizePty,kill}`, `fs/{readFile,writeFile,createDirectory,getMetadata,readDirectory,remove,copy,watch,unwatch}`, `mcpServer/*`.
- **Config / features**: `config/read`, `config/value/write`, `config/batchWrite`, `model/list`, `experimentalFeature/list`, `skills/list`, `app/list`, `plugin/*`.
- **Server-initiated notifications**: `thread/started`, `thread/status/changed`, `thread/tokenUsage/updated`, `turn/{started,completed,diff/updated,plan/updated}`, `item/{started,completed}` and `item/*/delta` streams, `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `account/{updated,rateLimits/updated,login/completed}`.
- **Item types** (`ThreadItem` union): `userMessage`, `agentMessage`, `plan`, `reasoning`, `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabToolCall`, `webSearch`, `imageView`, `enteredReviewMode`, `exitedReviewMode`, `contextCompaction`.

### Connection ladder

Client connection preference (broker → daemon → cold-spawn) and the refresh trigger are in [transcript.md → Appendix Codex](../transcript.md#appendix--codex).

## Rollout transcript JSONL

Codex writes one rollout file per session — its session log — at `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session_id>.jsonl`. The format is defined by the open-source `codex-rs` types (no standalone published schema; the path tree and event shapes are the **official source**, linked above). Two event shapes feed Rimz's context gauge:

```jsonc
// token usage
{ "type": "event_msg", "payload": { "type": "token_count",
    "info": { "model_context_window": <u64>,
              "last_token_usage": { "input_tokens": <u64>, "cached_input_tokens": <u64>,
                                    "output_tokens": <u64>, "total_tokens": <u64> } } } }

// model
{ "type": "turn_context", "payload": { "model": "gpt-5.5-codex" } }
```

Unlike Claude (raw tokens, window derived from the payload model), Codex carries the window directly (`model_context_window`), so the gauge is a precomputed `context_pct`. `last_token_usage` also feeds the card's per-call composition: `cached_input_tokens` is the `◌` cache-read figure, `input_tokens − cached_input_tokens` the `↘` fresh input (`input_tokens` includes the cached slice), and `output_tokens` the `↗` — the protocol reports no per-call cache-write, so the card grows no `◍`. The field → internal mapping and the date-tree walk (`RIMZ_CODEX_SESSIONS` overrides the root) are in [transcript.md](../transcript.md#appendix--codex).

## Auth file

[`account.rs`](../../../crates/rimz/src/agents/account.rs) reads `~/.codex/auth.json` directly (a cheap file read, no subprocess) for the logged-in-but-idle probe:

| Shape | Meaning |
| --- | --- |
| `OPENAI_API_KEY` present, non-empty | API-key login → **unmetered** (`∞`) |
| `tokens.access_token` present | ChatGPT login → **metered** (plan tier filled once a session reports it) |

The plan tier rides the app-server (`account/rateLimits/read` `plan_type`), not the idle file. The semantics are in [account.md](../account.md#per-provider-mapping).
