# Codex protocol reference

> The mapping onto RimZ's internal types lives beside this doc: [codex.md](../../internals/agents/codex.md) maps the hooks, rollout transcript, app-server, account, and spend surfaces onto RimZ's internal types; the agent-agnostic model is [model.md](../../internals/agents/model.md) and the account/spend model is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Codex upstream protocol surface** RimZ binds to — the hook events and their decision schema, the `notify` channel, the app-server JSON-RPC API, the rollout transcript, the auth file, and the local-OAuth usage endpoint. It is a hand-maintained mirror of OpenAI's published docs, the open-source `codex-rs` types, and the credential-file surfaces Codex itself uses, kept for fast lookup and pinned to the source URLs below. The [`CodexAdapter`](../../../crates/rimz/src/agents/adapters/codex/mod.rs) adapter and the [`codex::app_server`](../../../crates/rimz/src/agents/adapters/codex/app_server.rs) client are the only code that reads this surface.

Refresh baseline: Codex CLI **0.144.1** and the OpenAI Codex docs/source available on **2026-07-10**. Generated app-server details below come from `codex app-server generate-json-schema` on that release; the method index also calls out newer `main`-branch additions where stated.

Coverage is **depth on what RimZ wires, breadth as an index**: the events, app-server methods, and rollout fields the code actually parses or emits are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists.

## Upstream sources

Re-fetch these pages — and, for the app-server, re-run the schema generators — to refresh this mirror.

| Surface | Source |
| --- | --- |
| Hooks reference (events, payloads, decision schema, trust) | <https://learn.chatgpt.com/docs/hooks> |
| Hook executor (cwd/env semantics) | <https://github.com/openai/codex/blob/main/codex-rs/hooks/src/engine/command_runner.rs> |
| Config reference (`notify`, credential store, `[tui]` notifications) | <https://learn.chatgpt.com/docs/config-file/config-reference> |
| Advanced config (`notify` payload) | <https://learn.chatgpt.com/docs/config-file/config-advanced> |
| CLI reference (`resume`, `fork`, `login status`) | <https://learn.chatgpt.com/docs/developer-commands?surface=cli> |
| App-server API (protocol, methods, notifications) | <https://learn.chatgpt.com/docs/app-server> |
| App-server README + schema generation | <https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md> |
| App-server daemon lifecycle + PID backend | <https://github.com/openai/codex/blob/main/codex-rs/app-server-daemon/README.md>, <https://github.com/openai/codex/blob/main/codex-rs/app-server-daemon/src/lib.rs>, <https://github.com/openai/codex/blob/main/codex-rs/app-server-daemon/src/backend/pid.rs>, <https://github.com/openai/codex/blob/main/codex-rs/app-server-daemon/src/update_loop.rs> |
| App-server control socket WebSocket transport | <https://github.com/openai/codex/pull/21843> |
| Rollout/session JSONL + `auth.json` shape | open-source `codex-rs` types — <https://github.com/openai/codex> |
| OAuth usage endpoint | Codex credential-file traffic; no public schema page |

The app-server protocol has no published version string; the canonical, version-exact schema is generated from the Codex binary itself:

```bash
codex app-server generate-ts --out DIR           # TypeScript bindings
codex app-server generate-json-schema --out DIR  # JSON Schema bundle
```

## Session resume and fork

`codex resume <id>` reopens a session in place. `codex fork <id>` copies its conversation into a provider-assigned new session id and leaves the source session untouched; the interactive `fork` subcommand accepts no initial prompt, so RimZ opens the fork idle in the source worktree.

## Hooks

Codex hooks mirror Claude's shape: a command Codex runs at a lifecycle point, fed a JSON payload on **stdin**, returning a decision on **stdout**. They are wired in `~/.codex/config.toml` as `[[hooks.Event]]` tables. RimZ's [`CodexAdapter`](../../../crates/rimz/src/agents/adapters/codex/mod.rs) `INSTALLED_EVENTS` constant is the source of truth for the wired set; the native-event → RimZ status mapping is the [codex.md → Hooks and lifecycle](../../internals/agents/codex.md#hooks-and-lifecycle).

**Execution.** Matching groups from every active hook source run, and multiple matching command handlers for one event start concurrently. A hook command runs with the **session cwd** as working directory and the **spawning process's environment** (`command_runner.rs`: no `env_clear`, per-handler overlays only). Since 0.137 a plain TUI launch routes hooks through the shared per-user app-server daemon, so the hook child's parent — and its environment — is the daemon's, not the pane's; the mux-stamped identity pin never arrives via env, and RimZ recovers it from the in-pane process instead ([agent.md → Hooks resolve the room they live in](../../internals/agents/model.md#hooks-resolve-the-room-they-live-in)).

### Config shape

Codex discovers `hooks.json` and inline `[hooks]` tables beside active user and trusted-project config layers; plugin and managed layers can add their own hooks. Sources merge rather than replace one another. RimZ writes one user-level inline representation in `~/.codex/config.toml`, and hooks are enabled by default (`[features].hooks = false` disables them; `codex_hooks` is a deprecated alias).

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

Only `type = "command"` runs today. `prompt` and `agent` handlers are parsed and skipped, and `async = true` command handlers are also skipped. `UserPromptSubmit` and `Stop` ignore `matcher`; the installed RimZ groups omit it for those events.

### Trust state

Codex requires the user to review and trust each non-managed hook definition before it runs, records trust against the definition's hash, and **silently skips** a new or changed hook until it is trusted — `/hooks` inside Codex is the management UI (`--dangerously-bypass-hook-trust` bypasses for one invocation). Trust lands in the user config as `[hooks.state]` entries keyed `"<config-path>:<event_token>:<i>:<j>"`, the event token in lower_snake:

```toml
[hooks.state."/home/user/.codex/config.toml:permission_request:0:0"]
trusted_hash = "sha256:…"
```

A fresh `rimz hooks install` — or any change to the installed command — is therefore a wired-but-dead channel until the user trusts it inside Codex. RimZ detects the gap presence-only (`untrusted_hook_events_at` matches installed events against the state keys by token; the hash algorithm stays Codex's), and `rimz start`/`rimz doctor` surface the fix ([codex.md → Hooks and lifecycle](../../internals/agents/codex.md#hooks-and-lifecycle)).

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

RimZ parses around `permission_mode` without consuming it — the upstream still sends it; the agent model derives the turn phase from tool events instead.

Codex 0.144.4 also stamps optional `agent_id` and `agent_type` on `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, and `PostCompact` when the hook fires inside a child thread. A usable child observation has a non-empty `agent_id` distinct from the root `session_id`. Hooks expose neither the V2 generated nickname nor the canonical task path, and they carry no child token usage or assignment prompt; those are rollout/app-server data.

### Events and per-event input

| Event | Fires | Event-specific input | Wired |
| --- | --- | --- | :---: |
| `SessionStart` | session starts / resumes / clears / compacts | `source` (`startup`\|`resume`\|`clear`\|`compact`; `compact` is triggerless close evidence in RimZ) | ✓ |
| `UserPromptSubmit` | user submits a prompt | `turn_id`, `prompt` | ✓ |
| `SubagentStart` | a subagent launches | `turn_id`, `agent_id`, `agent_type`, `permission_mode` | ✓ |
| `PreToolUse` | before `exec_command` / `apply_patch` / MCP tools and the question pseudo-tool | `turn_id`, `tool_name`, `tool_use_id`, `tool_input`; `tool_name = "request_user_input"` is a user question; `update_plan` is non-blocking and is not a plan-approval gate | ✓ |
| `PermissionRequest` | approval needed (shell escalation, network) | `turn_id`, `tool_name`, `tool_input`, `tool_input.description?` | ✓ |
| `PostToolUse` | after tool output is produced | `turn_id`, `tool_name`, `tool_use_id`, `tool_input`, `tool_response`; `request_user_input` returns its id-keyed answers map | ✓ |
| `SubagentStop` | a subagent stops | `turn_id`, `agent_id`, `agent_type`, `agent_transcript_path`, `stop_hook_active`, `last_assistant_message` | ✓ |
| `Stop` | turn completes | `turn_id`, `stop_hook_active`, `last_assistant_message` | ✓ |
| `PreCompact` | before conversation compaction | `turn_id`, `trigger` (`manual`\|`auto`) | ✓ |
| `PostCompact` | after compaction | `turn_id`, `trigger` | ✓ |

Codex has **no `SessionEnd`, `Notification`, or dedicated plan-approval hook**. The client-side plan gate is derived from the rollout after the ordinary `Stop` hook; its wire shape is in [Plan mode, approval, and questions](#plan-mode-approval-and-questions). Compaction uses `PreCompact` as the opener; `PostCompact` closes with a known trigger, and a `SessionStart` with `source = "compact"` can still arrive as triggerless close evidence when `PostCompact` is missed.

Hook tool names are not the same vocabulary as rollout function-call names. The current hook contract reports canonical `Bash`, `apply_patch`, and `mcp__server__tool` names; `apply_patch` matcher aliases include `Edit` and `Write`. Hook interception remains partial: simple shell calls, `apply_patch`, and MCP calls are covered, while unified-exec, web search, and other non-shell/non-MCP paths are not a complete enforcement boundary. Current rollouts can still record `exec_command`, `apply_patch`, `update_plan`, and `request_user_input`, with older or compatibility traces mentioning `shell` / `local_shell`. RimZ reads the payload's actual `tool_name`, treats `request_user_input` as the only blocking `PreToolUse` question tool, and treats `update_plan` as ordinary non-blocking progress state.

**Observed registration quirks:** on Codex 0.144.1, opening a plain TUI reached the idle prompt without firing `SessionStart`; the hook rode the first submitted prompt immediately before `UserPromptSubmit`, and `/clear` provided no reliable `SessionStart(source = "clear")` observation despite that documented source value. Codex 0.144.5 now fires `SessionStart` on `/new` / conversation switch before the first `UserPromptSubmit`. RimZ still reads rollout `session_meta.payload.forked_from_id` for lineage: absent means a fresh `/clear` / `/new` root, present means a fork. RimZ's handling is in the [codex.md → Session registration](../../internals/agents/codex.md#session-registration-and-launch-quirks); re-verify these observations against an installed release on each refresh.

### Decision and output schema

RimZ emits Codex-native decisions for `PermissionRequest` and the blocking `PreToolUse` tools it owns:

```json
{ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": { "behavior": "allow|deny", "message": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": { "command": "string" } } }
{ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "string" } }
```

> **Divergence — never reuse Claude's shape.** A Codex `PermissionRequest` decision carries only `decision.behavior` and `message`. Emitting `updatedInput`, `updatedPermissions`, or `interrupt` corrupts it — those belong to *other* Codex hook types such as `PreToolUse`, not to a permission answer.

For reference, Codex's common-control and block shapes:

```json
// SessionStart / PreCompact / PostCompact / UserPromptSubmit / SubagentStop / Stop — common controls
{ "continue": true, "stopReason": "string", "systemMessage": "string", "suppressOutput": false, "hookSpecificOutput": { "additionalContext": "string" } }

// PostToolUse / Stop / SubagentStop — block
{ "decision": "block", "reason": "string" }
```

**Exit codes.** Exit `0` with JSON processes the output; exit `0` with no output continues; exit `2` is a blocking failure (stderr read as the reason/message). The neutral path RimZ takes is empty stdout, exit 0. Exact bytes are the inline goldens in [`codex/mod.rs`](../../../crates/rimz/src/agents/adapters/codex/mod.rs).

## `notify` channel

Independent of hooks, Codex can invoke an external program on supported events. RimZ uses **hooks, not `notify`** — this is recorded for reference. The `notify` key must live in the user-level `~/.codex/config.toml` (a project-local `notify` is ignored with a startup warning).

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

The external `notify` program currently receives only `agent-turn-complete`. `approval-requested` belongs to the separate in-terminal notification filter:

```toml
[tui]
notifications = true            # bool, or an array of event types to restrict to
notification_method = "auto"    # auto | osc9 | bel
notification_condition = "unfocused"  # unfocused | always
```

## App-server API

Codex has no statusline, so RimZ reads its rich context out of band from the **app-server**: a bidirectional JSON-RPC 2.0 service (the `"jsonrpc":"2.0"` header is omitted on the wire), streamed as JSONL over stdio by default. Transports: `stdio://` (default), experimental unsupported `ws://IP:PORT`, `unix://[PATH]`, or `off`. Start with `codex app-server` (or `--listen …`). The unix-domain control socket speaks standard WebSocket HTTP upgrade over that UDS, then carries the same JSON-RPC payloads as text frames. The server bounds ingress queues and returns retryable JSON-RPC error `-32001` when overloaded. A client must send one `initialize` request per connection, then an `initialized` notification, before any other method.

### Daemon lifecycle and stale-process recovery

`codex remote-control start` enables remote control and starts the persistent per-user app-server from `$CODEX_HOME/packages/standalone/current/codex`; `stop` requests its shutdown. The PID backend records `{ "pid", "processStartTime" }` in `$CODEX_HOME/app-server-daemon/app-server.pid` and `app-server-updater.pid`, where `processStartTime` is the trimmed `ps -p PID -o lstart=` value used as the PID-reuse guard. The updater process runs the exact `codex app-server daemon pid-update-loop` argv, and a remote-enabled child runs `codex app-server --remote-control --listen unix://`.

The updater waits five minutes before its first pass and one hour between later passes. Each pass runs the standalone installer and compares the updater's executable identity with the managed target. When they differ, it restarts a running app-server from the managed target and then replaces its own process image with that binary. `codex remote-control stop` stops only the app-server, and `start` preserves a live updater; that pair therefore does not repair updater skew. `codex app-server daemon bootstrap --remote-control` serializes the transition under the provider lifecycle lock, restarts the app-server, stops the existing updater, and starts a new updater from the managed target.

The 0.144.4 PID backend considers a recorded process active when `kill(pid, 0)` succeeds and its `ps` start time still matches. A zombie therefore remains active: `start` waits ten seconds for the absent control socket, while `stop` signals the zombie, waits a 60-second grace period, attempts `SIGKILL`, and reaches its 70-second timeout because signals cannot settle a dead child that its parent has not reaped. RimZ repairs only this proven state: the socket is absent; both structured PID records still match; the app-server is the updater's sole zombie child; both processes belong to the current user; and the updater executable and argv resolve inside the managed standalone install. It sends `SIGTERM` to that updater, waits up to two seconds for both identities to disappear, then retries the Codex control command once. Every ambiguous observation preserves the native Codex failure.

The protocol is organized around three primitives: an **Item** (atomic input/output unit with a `started` → optional `delta` → `completed` lifecycle), a **Turn** (the items from one unit of agent work), and a **Thread** (the durable session container).

### Methods RimZ uses

The [`codex::app_server`](../../../crates/rimz/src/agents/adapters/codex/app_server.rs) client speaks only **read-only, non-interfering** methods — it never calls `thread/resume`, `turn/start`, or any write, which would rejoin and own the user's live thread.

**`thread/loaded/list`** → the thread ids the app-server currently holds in memory; the daemon-mode liveness signal RimZ reaps ghost sessions against ([sidebar.md → Presence model](../../internals/sidebar/sidebar.md#presence-model)).

```jsonc
// result — v2/ThreadLoadedListResponse.json from `codex app-server generate-json-schema`
{ "data": ["string", …], "nextCursor": "string | null" }
```

The reaper queries the per-user daemon **specifically** (never a cold-spawn, whose empty set would mass-reap), sends `{}` as params, follows `nextCursor`, and trusts only a recognized id-list shape: a response with no id field is treated as unknown, not zero, so a wire-shape drift keeps every session. RimZ still accepts the older `threadIds`, `threads`, `loadedThreadIds`, `ids`, and bare-array shapes for compatibility. The set is loaded-in-memory, not attached-pane, so it is a liveness improvement, not a perfect pane signal.

**`initialize`** → handshake; the response `userAgent` carries the Codex version.

```jsonc
// request
{ "method": "initialize", "params": { "clientInfo": { "name": "rimz", "version": "x.y.z" } } }
// then, as a notification:
{ "method": "initialized", "params": {} }
```

**`account/rateLimits/read`** → the included-usage windows, plan tier, optional paid-credit state, and optional rate-limit reset credits.

```jsonc
// result — RateLimitsResponse { rateLimits: RateLimitSnapshot }
{
  "rateLimits": {
    "primary":   { "usedPercent": 0-100, "resetsAt": <epoch s>, "windowDurationMins": 300 },   // ~5h
    "secondary": { "usedPercent": 0-100, "resetsAt": <epoch s>, "windowDurationMins": 10080 }, // ~7d
    "planType": "plus | pro | team | …",
    "credits": {
      "hasCredits": true | false,          // optional, mapped
      "unlimited": true | false,           // optional, mapped
      "overageLimitReached": true | false, // optional, mapped
      "balance": <USD number or string>    // optional, mapped
    } // optional, tolerated here or at the result root
  },
  "credits": {
    "hasCredits": true | false,
    "unlimited": true | false,
    "overageLimitReached": true | false,
    "balance": <USD number or string>
  }, // optional
  "rateLimitsByLimitId": { "codex": { "primary": {}, "secondary": {} } }, // optional multi-bucket view
  "rateLimitResetCredits": {
    "availableCount": 2,
    "credits": [
      { "id": "opaque", "status": "available", "expiresAt": <epoch s>, "grantedAt": <epoch s>, "resetType": "codexRateLimits" }
    ]
  } // optional; credits may be null when only the count is known
}
```

Fields are `camelCase` on the wire (`#[serde(rename_all = "camelCase")]`); `secondary` may be `null`, and reported window lengths render from `windowDurationMins`. Codex declares one product-level exception: when an authoritative response reports another window but omits the 5-hour duration, RimZ keeps the 5-hour slot visible as an unlimited `∞` bar. `rateLimits` remains the backward-compatible single-bucket view; `rateLimitsByLimitId` carries newer multi-bucket data. The optional `credits` object is mapped by the shared Codex credit rule: `overageLimitReached: true` means exhausted in older payloads, `unlimited: true` means usable with unknown remaining balance, numeric/string `balance` means remaining USD, and `hasCredits: false` means disabled. The 0.144.1 generated schema requires only `hasCredits` and `unlimited` in `CreditsSnapshot`; RimZ tolerates the older fields and unknown shapes without dropping valid windows. `rateLimitResetCredits.availableCount` maps to the authoritative dashboard count; every valid `expiresAt` among `available` detail rows is retained, and the earliest remains the summary expiry.

**`model/list`** (`{ "includeHidden": true }`) → the session model's display name. The payload also carries `defaultReasoningEffort`, but RimZ does not map it to row effort because it is a catalog default/recommendation, not the current session's live value.

```jsonc
// result.data[] (RawModel)
{ "id": "string", "model": "string", "displayName": "string", "defaultReasoningEffort": "string?" }
```

**`thread/read`** (`{ "threadId": "<session_id>", "includeTurns": false }`) and **`thread/list`** → stored thread metadata for the card description.

```jsonc
// thread/read result (wrapped shape; direct thread objects are also tolerated)
{ "thread": { "id": "thr_123", "preview": "Create a TUI", "name": "TUI prototype" } }

// thread/list result.data[]
{ "id": "thr_123", "preview": "Create a TUI", "name": "TUI prototype", "updatedAt": 1730831111 }
```

RimZ reads `thread/read` by the hook `session_id`, then uses `thread/list` as the documented list-summary fallback to fill missing thread metadata, matching by `id` or `sessionId`. `preview` maps to `AgentContext.session_preview` and wins the Codex card's description line; `name` maps to `AgentContext.session_name` as the thread-name fallback when no preview exists.

**The token-usage gap.** The app-server does **not** expose token / context-window usage read-only — it rides only the live `thread/tokenUsage/updated` notification behind a subscribing `thread/resume`. So Codex's context gauge is sourced from the rollout transcript below, not the app-server.

### Method index (the rest)

A non-exhaustive map of the broader surface, for future wiring. Generate the exact, version-pinned schema with `codex app-server generate-json-schema`.

- **Thread**: `thread/start`, `thread/resume`, `thread/fork`, `thread/archive`, `thread/name/set`, `thread/goal/{set,get,clear}`, `thread/compact/start`, `thread/rollback`, `thread/inject_items`, `thread/metadata/update`; current `main` also documents experimental `thread/turns/list`, while 0.144.1 does not generate it.
- **Turn**: `turn/start`, `turn/steer`, `turn/interrupt`; `review/start`.
- **Account / auth**: `account/read`, `account/login/{start,cancel}`, `account/logout`, `account/rateLimits/read`, `account/usage/read`, `account/rateLimitResetCredit/consume`.
- **Tools / exec / fs**: `command/exec` (+ `write`/`resize`/`terminate`), `process/{spawn,writeStdin,resizePty,kill}`, `fs/{readFile,writeFile,createDirectory,getMetadata,readDirectory,remove,copy,watch,unwatch}`, `mcpServer/*`.
- **Config / features**: `config/read`, `config/value/write`, `config/batchWrite`, `configRequirements/read`, `model/list`, `experimentalFeature/list`, `skills/list`, `hooks/list`, `app/list`, `plugin/*`.
- **Server-initiated notifications**: `thread/started`, `thread/status/changed`, `thread/tokenUsage/updated`, `turn/{started,completed,diff/updated,plan/updated}`, `item/{started,completed}` and `item/*/delta` streams, `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `account/{updated,rateLimits/updated,login/completed}`.
- **Item types** (`ThreadItem` union): `userMessage`, `agentMessage`, `plan`, `reasoning`, `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabToolCall`, `webSearch`, `imageView`, `enteredReviewMode`, `exitedReviewMode`, `contextCompaction`.

### Connection ladder

Client connection preference (broker → daemon → cold-spawn) and the refresh trigger are in [codex.md → Context and transcript](../../internals/agents/codex.md#context-and-transcript).

## Rollout transcript JSONL

Codex writes one rollout file per session — its session log — at `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session_id>.jsonl`, and moves archived rollouts into the sibling `~/.codex/archived_sessions/` tree. The format is defined by the open-source `codex-rs` types (no standalone published schema; the path tree and event shapes are the **official source**, linked above). Rollout events feed RimZ's context gauge, supervised-run streaming, plan approval, and the local turn-settle markers:

```jsonc
// V2 session metadata; forked_from_id appears on user-created forks
{ "type": "session_meta", "payload": {
    "id": "<session_id>", "cwd": "/repo", "thread_source": "user" | "subagent",
    "forked_from_id": "<fork_parent>", "parent_thread_id": "<immediate_parent>",
    "agent_nickname": "Atlas", "agent_path": "/root/research/explore_hooks",
    "agent_role": "explorer", "multi_agent_version": "v2" } }

// older structured child source, still observed in copied rollout headers
{ "type": "session_meta", "payload": { "id": "<child_id>",
    "source": { "subagent": { "thread_spawn": {
      "parent_thread_id": "<immediate_parent>", "depth": 2,
      "agent_path": "/root/research/explore_hooks",
      "agent_nickname": "Atlas", "agent_role": "explorer" } } } } }

// token usage
{ "type": "event_msg", "payload": { "type": "token_count",
    "info": { "model_context_window": <u64>,
              "last_token_usage": { "input_tokens": <u64>, "cached_input_tokens": <u64>,
                                    "output_tokens": <u64>, "total_tokens": <u64> } } } }

// model and reasoning effort
{ "type": "turn_context", "payload": { "model": "gpt-5.5-codex", "effort": "xhigh" } }

// assistant stream message
{ "type": "event_msg", "payload": { "type": "agent_message", "message": "..." } }

// provider turn error — accepted variants, classified through the app-server TurnError vocabulary
{ "timestamp": "2026-06-11T07:18:00.000Z",
  "type": "event_msg",
  "payload": { "type": "turn_error" | "stream_error" | "error",
               "message": "You've hit your usage limit",
               "codexErrorInfo": "usageLimitExceeded" | "serverOverloaded" | "internalServerError" | "..." } }
{ "timestamp": "2026-06-11T07:18:00.000Z",
  "type": "event_msg",
  "payload": { "type": "task_complete",
               "error": { "message": "API Error: Server Error",
                          "codexErrorInfo": "internalServerError" } } }

// clean task completion; observed resting successes carry last_agent_message text
{ "timestamp": "2026-06-14T05:59:49.268Z",
  "type": "event_msg",
  "payload": { "type": "task_complete",
               "last_agent_message": "patch is correct" } }

// interrupted turn; observed on Esc and on /clear of a running turn, with no Stop hook
{ "timestamp": "2026-07-07T14:12:00.000Z",
  "type": "event_msg",
  "payload": { "type": "turn_aborted",
               "reason": "interrupted",
               "turn_id": "turn-1",
               "completed_at": "2026-07-07T14:12:00.000Z" } }
```

Codex carries the window directly (`model_context_window`); RimZ derives occupancy from the bounded `last_token_usage` reading. For a child this value is current context/request usage, not lifetime spend. `session_meta.payload.forked_from_id` carries fork lineage for `/side` / `/btw` / `/fork`; a parentless head is a fresh root such as `/clear` / `/new`. `thread_source = "subagent"` or the structured `source.subagent.thread_spawn` object positively identifies a child; `forked_from_id` alone still identifies user forks and is not child proof. The direct V2 fields supply nickname, root-relative task path, role, immediate parent, and version, with the structured spawn fields as tolerant fallbacks. Fork and subagent rollouts copy the parent's historical token-count records into a single timestamp second before appending their own work, so spend readers retain that cumulative prefix as a baseline and suppress it as billable usage. `last_token_usage` also feeds the card's per-call composition: `cached_input_tokens` is the `◌` cache-read figure, `input_tokens − cached_input_tokens` the `↘` fresh input (`input_tokens` includes the cached slice), and `output_tokens` the `↗` — the protocol reports no per-call cache-write, so the card grows no `◍`. `agent_message.message` is the main-thread assistant text RimZ emits as `rimz agents <spec> -p --stream` / `rimz agents wait --stream` progress; duplicate `response_item` rows are ignored for streaming. Error records use the app-server `TurnError` vocabulary generated by `codex app-server generate-json-schema --out DIR`: `codexErrorInfo = usageLimitExceeded` pauses for a rate limit, `serverOverloaded` and `internalServerError` pause for the backoff class, and other known variants fail the row. Label fallback maps "spend limit" to the spend-limit paused class; "usage limit", "session limit", "rate limit", "quota", and "too many requests" map to the rate-limit paused class; "at capacity" maps to the overload backoff class. Observed Codex 0.142.x serving-capacity failures render `⚠ Selected model is at capacity. Please try a different model.` in the TUI only; observed usage-limit failures render `■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage … try again at 6:35 AM.` in the TUI only. Both shapes leave the rollout resting on `event_msg` / `task_complete` with `last_agent_message: null`, no `error` field, no `Stop` hook, and no app-server `thread/read` error field; upstream issue threads include openai/codex #22277, #19579, #28507, and #29760. RimZ matches banner keywords, never glyphs, so `⚠`, `■`, and future ornaments are not protocol. Observed interrupted turns render `event_msg` / `turn_aborted` with `reason: "interrupted"` on Esc and `/clear` of a running turn, and no `Stop` hook; RimZ treats any resting `turn_aborted` reason as an interrupted marker and lets a later live record clear it. The field → internal mapping, date-tree walk (`RIMZ_CODEX_SESSIONS` overrides the root), and self-clear rule are in [codex.md → Context and transcript](../../internals/agents/codex.md#context-and-transcript).

### Plan mode, approval, and questions

Plan-mode shapes were verified against local Codex 0.144.3 rollouts and the installed TUI on 2026-07-13. A plan turn identifies its collaboration mode in `turn_context.payload.collaboration_mode.mode = "plan"` and `task_started.payload.collaboration_mode_kind = "plan"`; hook payloads report `permission_mode = "plan"`. The authoritative approval evidence is the completed item and same-turn clean completion:

```jsonc
{ "timestamp": "2026-07-13T10:00:01Z", "type": "event_msg",
  "payload": { "type": "item_completed", "turn_id": "turn-1",
    "item": { "type": "Plan", "id": "turn-1-plan", "text": "# Plan\n\n..." } } }
{ "timestamp": "2026-07-13T10:00:03Z", "type": "event_msg",
  "payload": { "type": "task_complete", "turn_id": "turn-1", "last_agent_message": "Codex says:" } }
```

The parallel assistant response item wraps the body in `<proposed_plan>…</proposed_plan>`, while `task_complete.last_agent_message` excludes it. The TUI draws “Implement this plan?” only after the turn, with “Yes, implement this plan”, “Yes, clear context and implement”, and “No, stay in Plan mode”; the first row switches to Default mode and submits `Implement the plan.`. This selector is client-side and emits no dedicated hook or `notify` event ([openai/codex#19921](https://github.com/openai/codex/issues/19921)); the version-pinned implementation strings and actions live in [`plan_implementation.rs`](https://github.com/openai/codex/blob/78ad6e6bfd1d3b6a209acd3ef82172a96b25179c/codex-rs/tui/src/chatwidget/plan_implementation.rs). RimZ therefore derives the ask from the rollout at the ordinary `Stop` boundary.

Plan clarifications and default-mode questionnaires use the same `request_user_input` tool. Verified hook input and output:

```jsonc
// PreToolUse.tool_input
{ "questions": [
  { "id": "path", "header": "Migration", "question": "Pick a path?",
    "options": [
      { "label": "Blue", "description": "Safer rollout" },
      { "label": "Green", "description": "Faster rollout" }
    ] }
] }

// PostToolUse.tool_response
{ "answers": { "path": { "answers": ["Blue"] } } }
```

The questionnaire starts each option list on its first row; Down moves selection and Enter commits, advances, and submits on the final question. The current UI also supports notes/custom text, while RimZ tolerantly parses `multi_select` and `multiSelect` if a producer supplies either spelling. The version-pinned interaction state machine lives in [`request_user_input/mod.rs`](https://github.com/openai/codex/blob/78ad6e6bfd1d3b6a209acd3ef82172a96b25179c/codex-rs/tui/src/bottom_pane/request_user_input/mod.rs).

## Auth file

Codex stores credentials according to `cli_auth_credentials_store = "file" | "keyring" | "auto"`. With file storage, [`account.rs`](../../../crates/rimz/src/agents/adapters/codex/account.rs) reads `~/.codex/auth.json` directly for the logged-in-but-idle probe:

| Shape | Meaning |
| --- | --- |
| `OPENAI_API_KEY` present, non-empty | API-key login → **unmetered** by subscription windows; the provider dashboard uses transcript-derived API spend plus any display ceiling |
| `tokens.access_token` present | ChatGPT login → **metered** (plan tier filled by live app-server context or the OAuth usage response) |
| `tokens.account_id` present, non-empty | explicit ChatGPT account identity copied to `AgentAccount.account_id`, the `ChatGPT-Account-Id` request header, and OAuth cache ownership |

When no auth file exists, RimZ runs `codex login status` so keyring-backed logins still appear with the correct metered/unmetered posture. The command prints one line per auth mode: `Logged in using ChatGPT` (metered by subscription windows), `Logged in using an API key - <masked>` and `Logged in using Amazon Bedrock API key` (token/AWS-billed, so unmetered), `Logged in using access token` and `Logged in using personal access token` (logged in, metering unknown), or `Not logged in`. It reports login kind but no plan tier or token, so the plan rides the app-server (`account/rateLimits/read` `planType`) and direct OAuth usage remains available only when Codex exposes a file token. The semantics are in [codex.md → Account and balance](../../internals/agents/codex.md#account-and-balance).

[`oauth_usage.rs`](../../../crates/rimz/src/agents/adapters/codex/oauth_usage.rs) uses the same `tokens.access_token` for the direct account-usage probe. An API-key-only auth file has no OAuth endpoint and skips this path. When `tokens.account_id` is present, the request also sends `ChatGPT-Account-Id`; the same trimmed explicit field identifies the idle `AgentAccount` and the successful usage observation, so a stale preflight read cannot assign fetched facts to the wrong cache owner. RimZ does not decode JWT claims for identity.

The app-server `account/rateLimits/read` `planType` is persisted with realtime credits even when plan is the only account field returned. A non-empty app-server plan replaces the cached OAuth plan; an absent app-server plan preserves it, so keyring-backed and idle sessions retain their last provider-authoritative label.

The default usage URL is `GET https://chatgpt.com/backend-api/wham/usage`. A `chatgpt_base_url` value in `~/.codex/config.toml` overrides the base: bases ending in `/backend-api` append `/wham/usage`; other bases append `/api/codex/usage`. The parsed usage response shape:

```jsonc
{
  "user_id": "user_…",     // present, ignored
  "account_id": "acct_…",  // present, ignored; local tokens.account_id keys account switches
  "email": "person@example.com", // present, ignored
  "plan_type": "plus | pro | team | …", // mapped as the idle/switched-account plan label fallback
  "rate_limit": {
    "primary_window": {
      "used_percent": 0-100,           // mapped
      "reset_at": <epoch s>,           // mapped
      "limit_window_seconds": 18000,   // mapped to duration_mins
      "reset_after_seconds": 123       // present, ignored
    },
    "secondary_window": {
      "used_percent": 0-100,           // mapped
      "reset_at": <epoch s>,           // mapped
      "limit_window_seconds": 604800,  // mapped to duration_mins
      "reset_after_seconds": 123       // present, ignored
    }
  },
  "credits": {
    "has_credits": true | false,            // mapped
    "unlimited": true | false,              // mapped
    "overage_limit_reached": true | false,  // mapped
    "balance": <USD number, string, or null>, // mapped when numeric/string
    "approx_local_messages": null,          // present, ignored
    "approx_cloud_messages": null           // present, ignored
  },
  "spend_control": {},                 // present, ignored
  "rate_limit_reset_credits": null     // present, ignored
}
```

Each window's `limit_window_seconds` maps to `duration_mins`; primary/secondary order is not semantic. Codex credits map in this order: `overage_limit_reached: true` → exhausted `ExtraCredits::Known { remaining_usd: 0 }`, `unlimited: true` → usable `ExtraCredits::Known` with unknown remaining balance, numeric/string `balance` → `ExtraCredits::Known { remaining_usd }`, `has_credits: false` → `ExtraCredits::Disabled`, and omitted/unknown fields → no extra-credit reading.

Rate-limit reset credits use the same OAuth token and optional `ChatGPT-Account-Id` header, fetched from `GET https://chatgpt.com/backend-api/wham/rate-limit-reset-credits`; with a non-`/backend-api` base the path is `/api/codex/rate-limit-reset-credits`. The parsed response shape:

```jsonc
{
  "available_count": 2, // mapped to ResetCredits.count when present
  "credits": [
    {
      "status": "available",             // only available credits affect the count and expiry
      "expires_at": "2026-07-06T12:00:00Z", // retained in expiries; earliest also becomes soonest_expiry
      "title": "Rate Limit Reset"        // present, ignored
    }
  ]
}
```

The usage response's `rate_limit_reset_credits` field remains ignored; the dedicated endpoint is the per-credit source and rides the standard OAuth usage cadence. Available detail expiries that fail RFC 3339 parsing are excluded without changing `available_count`; valid expiries sort ascending and equal timestamps remain distinct credits. A reset-credit fetch failure leaves the prior dashboard value in place when the primary usage fetch succeeds.

RimZ consumes a credit with `POST https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume`, or `/api/codex/rate-limit-reset-credits/consume` for a non-`/backend-api` base, using the same OAuth and account headers as the GET. The request is `{"redeem_request_id":"<uuid-v7>","credit_id":"<optional id>"}`; the response carries `code` (`reset`, `nothing_to_reset`, `no_credit`, or `already_redeemed`) and `windows_reset`. Unknown codes remain a non-success outcome for forward compatibility. `nothing_to_reset` leaves the credit available.
