# Codex protocol reference

> The mapping onto Rimz's internal types lives beside this doc: [adapter/codex.md](../../internals/agents/adapter/codex.md) maps the hooks, rollout transcript, app-server, account, and spend surfaces onto Rimz's internal types; the agent-agnostic model is [agent.md](../../internals/agents/agent.md) and the account/spend model is [provider.md](../../internals/agents/provider.md).

This is the single home for the **Codex upstream protocol surface** Rimz binds to — the hook events and their decision schema, the `notify` channel, the app-server JSON-RPC API, the rollout transcript, the auth file, and the local-OAuth usage endpoint. It is a hand-maintained mirror of OpenAI's published docs, the open-source `codex-rs` types, and the credential-file surfaces Codex itself uses, kept for fast lookup and pinned to the source URLs below. The [`CodexAdapter`](../../../crates/rimz/src/agents/codex/mod.rs) adapter and the [`codex::app_server`](../../../crates/rimz/src/agents/codex/app_server.rs) client are the only code that reads this surface.

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
| App-server control socket WebSocket transport | <https://github.com/openai/codex/pull/21843> |
| Rollout/session JSONL + `auth.json` shape | open-source `codex-rs` types — <https://github.com/openai/codex> |
| OAuth usage endpoint | Codex credential-file traffic; no public schema page |

The app-server protocol has no published version string; the canonical, version-exact schema is generated from the Codex binary itself:

```bash
codex app-server generate-ts --out DIR           # TypeScript bindings
codex app-server generate-json-schema --out DIR  # JSON Schema bundle
```

## Hooks

Codex hooks mirror Claude's shape: a command Codex runs at a lifecycle point, fed a JSON payload on **stdin**, returning a decision on **stdout**. They are wired in `~/.codex/config.toml` as `[[hooks.Event]]` tables. Rimz's [`CodexAdapter`](../../../crates/rimz/src/agents/codex/mod.rs) `INSTALLED_EVENTS` constant is the source of truth for the wired set; the native-event → Rimz status mapping is the [adapter/codex.md → Hooks and lifecycle](../../internals/agents/adapter/codex.md#hooks-and-lifecycle).

**Execution.** A hook command runs with the **session cwd** as working directory and the **spawning process's environment** (`command_runner.rs`: no `env_clear`, per-handler overlays only). Since 0.137 a plain TUI launch routes hooks through the shared per-user app-server daemon, so the hook child's parent — and its environment — is the daemon's, not the pane's; the mux-stamped identity pin never arrives via env, and Rimz recovers it from the in-pane process instead ([agent.md → Hooks resolve the room they live in](../../internals/agents/agent.md#hooks-resolve-the-room-they-live-in)).

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

A fresh `rimz hooks install` — or any change to the installed command — is therefore a wired-but-dead channel until the user trusts it inside Codex. Rimz detects the gap presence-only (`untrusted_hook_events_at` matches installed events against the state keys by token; the hash algorithm stays Codex's), and `rimz start`/`rimz doctor` surface the fix ([adapter/codex.md → Hooks and lifecycle](../../internals/agents/adapter/codex.md#hooks-and-lifecycle)).

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
| `SessionStart` | session starts / resumes / clears / compacts | `source` (`startup`\|`resume`\|`clear`\|`compact`; `compact` is triggerless close evidence in Rimz) | ✓ |
| `UserPromptSubmit` | user submits a prompt | `turn_id`, `prompt` | ✓ |
| `SubagentStart` | a subagent launches | `turn_id`, `agent_id`, `agent_type`, `permission_mode` | ✓ |
| `PreToolUse` | before `exec_command` / `apply_patch` / MCP tools and the question pseudo-tool | `turn_id`, `tool_name`, `tool_use_id`, `tool_input`; `tool_name = "request_user_input"` is a user question; `update_plan` is non-blocking and is not a plan-approval gate | ✓ |
| `PermissionRequest` | approval needed (shell escalation, network) | `turn_id`, `tool_name`, `tool_input`, `tool_input.description?` | ✓ |
| `PostToolUse` | after tool output is produced | `turn_id`, `tool_name`, `tool_use_id`, `tool_input`, `tool_response` | ✓ |
| `SubagentStop` | a subagent stops | `turn_id`, `agent_id`, `agent_type`, `agent_transcript_path`, `stop_hook_active`, `last_assistant_message` | ✓ |
| `Stop` | turn completes | `turn_id`, `stop_hook_active`, `last_assistant_message` | ✓ |
| `PreCompact` | before conversation compaction | `turn_id`, `trigger` (`manual`\|`auto`) | ✓ |
| `PostCompact` | after compaction | `turn_id`, `trigger` | ✓ |

Codex has **no `SessionEnd`, `Notification`, or plan-approval hook**. Compaction uses `PreCompact` as the opener; `PostCompact` closes with a known trigger, and a `SessionStart` with `source = "compact"` can still arrive as triggerless close evidence when `PostCompact` is missed.

Observed Codex tool names in current rollouts include `exec_command`, `apply_patch`, `update_plan`, and `request_user_input`; older or compatibility traces can still mention `shell` / `local_shell`. Rimz treats `request_user_input` as the only blocking `PreToolUse` question tool and treats `update_plan` as ordinary non-blocking progress state.

**Observed registration quirks** (upstream, pinned for refresh): `SessionStart` does not fire on a plain CLI launch — it rides the first `UserPromptSubmit` — and does not fire on `/clear` despite the documented `source = "clear"`. Rimz works around the missing clear hook through rollout `session_meta.payload.forked_from_id`: absent means a fresh `/clear` / `/new` root, present means a fork. Rimz's handling is in the [adapter/codex.md → Session registration](../../internals/agents/adapter/codex.md#session-registration-and-launch-quirks); re-verify against the hooks reference URL above on each refresh.

### Decision and output schema

Rimz emits Codex-native decisions for `PermissionRequest` and the blocking `PreToolUse` tools it owns:

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

Codex has no statusline, so Rimz reads its rich context out of band from the **app-server**: a bidirectional JSON-RPC 2.0 service (the `"jsonrpc":"2.0"` header is omitted on the wire), streamed as JSONL over stdio by default. Transports: `stdio://` (default), `ws://IP:PORT`, `unix://[PATH]`, or `off`. Start with `codex app-server` (or `--listen …`). The remote-control daemon's unix-domain control socket speaks standard WebSocket HTTP upgrade over that UDS, then carries the same JSON-RPC payloads as text frames. A client must send one `initialize` request per connection, then an `initialized` notification, before any other method.

The protocol is organized around three primitives: an **Item** (atomic input/output unit with a `started` → optional `delta` → `completed` lifecycle), a **Turn** (the items from one unit of agent work), and a **Thread** (the durable session container).

### Methods Rimz uses

The [`codex::app_server`](../../../crates/rimz/src/agents/codex/app_server.rs) client speaks only **read-only, non-interfering** methods — it never calls `thread/resume`, `turn/start`, or any write, which would rejoin and own the user's live thread.

**`thread/loaded/list`** → the thread ids the app-server currently holds in memory; the daemon-mode liveness signal Rimz reaps ghost sessions against ([sidebar.md → Presence model](../../internals/sidebar/sidebar.md#presence-model)).

```jsonc
// result — v2/ThreadLoadedListResponse.json from `codex app-server generate-json-schema`
{ "data": ["string", …], "nextCursor": "string | null" }
```

The reaper queries the per-user daemon **specifically** (never a cold-spawn, whose empty set would mass-reap), sends `{}` as params, follows `nextCursor`, and trusts only a recognized id-list shape: a response with no id field is treated as unknown, not zero, so a wire-shape drift keeps every session. Rimz still accepts the older `threadIds`, `threads`, `loadedThreadIds`, `ids`, and bare-array shapes for compatibility. The set is loaded-in-memory, not attached-pane, so it is a liveness improvement, not a perfect pane signal.

**`initialize`** → handshake; the response `userAgent` carries the Codex version.

```jsonc
// request
{ "method": "initialize", "params": { "clientInfo": { "name": "rimz", "version": "x.y.z" } } }
// then, as a notification:
{ "method": "initialized", "params": {} }
```

**`account/rateLimits/read`** → the 5h/7d balance windows, plan tier, and optional paid-credit state.

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
  } // optional
}
```

Fields are `camelCase` on the wire (`#[serde(rename_all = "camelCase")]`); `secondary` may be `null`, and a server-side change in window count or length renders gracefully off `windowDurationMins` rather than a hard-coded 5h/7d. The optional `credits` object is mapped by the shared Codex credit rule: `overageLimitReached: true` means exhausted, `unlimited: true` means usable with unknown remaining balance, numeric/string `balance` means remaining USD, and `hasCredits: false` means disabled. Snake-case aliases are tolerated for the credit-state fields, and unknown credit shapes leave valid windows intact.

**`model/list`** (`{ "includeHidden": true }`) → the session model's display name. The payload also carries `defaultReasoningEffort`, but Rimz does not map it to row effort because it is a catalog default/recommendation, not the current session's live value.

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

Rimz reads `thread/read` by the hook `session_id`, then uses `thread/list` as the documented list-summary fallback to fill missing thread metadata, matching by `id` or `sessionId`. `preview` maps to `AgentContext.session_preview` and wins the Codex card's description line; `name` maps to `AgentContext.session_name` as the thread-name fallback when no preview exists.

**The token-usage gap.** The app-server does **not** expose token / context-window usage read-only — it rides only the live `thread/tokenUsage/updated` notification behind a subscribing `thread/resume`. So Codex's context gauge is sourced from the rollout transcript below, not the app-server.

### Method index (the rest)

A non-exhaustive map of the broader surface, for future wiring. Generate the exact, version-pinned schema with `codex app-server generate-json-schema`.

- **Thread**: `thread/start`, `thread/resume`, `thread/fork`, `thread/archive`, `thread/name/set`, `thread/goal/{set,get,clear}`, `thread/compact/start`, `thread/rollback`, `thread/inject_items`.
- **Turn**: `turn/start`, `turn/steer`, `turn/interrupt`; `review/start`.
- **Account / auth**: `account/read`, `account/login/{start,cancel}`, `account/logout`, `account/rateLimits/read`, `account/sendAddCreditsNudgeEmail`.
- **Tools / exec / fs**: `command/exec` (+ `write`/`resize`/`terminate`), `process/{spawn,writeStdin,resizePty,kill}`, `fs/{readFile,writeFile,createDirectory,getMetadata,readDirectory,remove,copy,watch,unwatch}`, `mcpServer/*`.
- **Config / features**: `config/read`, `config/value/write`, `config/batchWrite`, `model/list`, `experimentalFeature/list`, `skills/list`, `app/list`, `plugin/*`.
- **Server-initiated notifications**: `thread/started`, `thread/status/changed`, `thread/tokenUsage/updated`, `turn/{started,completed,diff/updated,plan/updated}`, `item/{started,completed}` and `item/*/delta` streams, `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `account/{updated,rateLimits/updated,login/completed}`.
- **Item types** (`ThreadItem` union): `userMessage`, `agentMessage`, `plan`, `reasoning`, `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabToolCall`, `webSearch`, `imageView`, `enteredReviewMode`, `exitedReviewMode`, `contextCompaction`.

### Connection ladder

Client connection preference (broker → daemon → cold-spawn) and the refresh trigger are in [adapter/codex.md → Context and transcript](../../internals/agents/adapter/codex.md#context-and-transcript).

## Rollout transcript JSONL

Codex writes one rollout file per session — its session log — at `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session_id>.jsonl`. The format is defined by the open-source `codex-rs` types (no standalone published schema; the path tree and event shapes are the **official source**, linked above). Rollout events feed Rimz's context gauge, supervised-run streaming, and the local turn-death marker:

```jsonc
// session metadata; forked_from_id appears on /side, /btw, and /fork children
{ "type": "session_meta", "payload": { "id": "<session_id>", "cwd": "/repo", "thread_source": "user" | "subagent", "forked_from_id": "<parent_session_id>" } }

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

Unlike Claude (raw tokens, window derived from the payload model), Codex carries the window directly (`model_context_window`), so the gauge is a precomputed `context_pct`. `session_meta.payload.forked_from_id` carries fork lineage for `/side` / `/btw` / `/fork`; a parentless head is a fresh root such as `/clear` / `/new`. `thread_source` identifies `user` and `subagent` origins. `last_token_usage` also feeds the card's per-call composition: `cached_input_tokens` is the `◌` cache-read figure, `input_tokens − cached_input_tokens` the `↘` fresh input (`input_tokens` includes the cached slice), and `output_tokens` the `↗` — the protocol reports no per-call cache-write, so the card grows no `◍`. `agent_message.message` is the main-thread assistant text Rimz emits as `rimz agents <spec> -p --stream` / `rimz agents wait --stream` progress; duplicate `response_item` rows are ignored for streaming. Error records use the app-server `TurnError` vocabulary generated by `codex app-server generate-json-schema --out DIR`: `codexErrorInfo = usageLimitExceeded` pauses for a rate limit, `serverOverloaded` and `internalServerError` pause for the backoff class, and other known variants fail the row. Label fallback maps "spend limit" to the spend-limit paused class; "usage limit", "session limit", "rate limit", "quota", and "too many requests" map to the rate-limit paused class; "at capacity" maps to the overload backoff class. Observed Codex 0.142.x serving-capacity failures render `⚠ Selected model is at capacity. Please try a different model.` in the TUI only; observed usage-limit failures render `■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage … try again at 6:35 AM.` in the TUI only. Both shapes leave the rollout resting on `event_msg` / `task_complete` with `last_agent_message: null`, no `error` field, no `Stop` hook, and no app-server `thread/read` error field; upstream issue threads include openai/codex #22277, #19579, #28507, and #29760. Rimz matches banner keywords, never glyphs, so `⚠`, `■`, and future ornaments are not protocol. Observed interrupted turns render `event_msg` / `turn_aborted` with `reason: "interrupted"` on Esc and `/clear` of a running turn, and no `Stop` hook; Rimz treats any resting `turn_aborted` reason as an interrupted marker and lets a later live record clear it. The field → internal mapping, date-tree walk (`RIMZ_CODEX_SESSIONS` overrides the root), and self-clear rule are in [adapter/codex.md → Context and transcript](../../internals/agents/adapter/codex.md#context-and-transcript).

## Auth file

[`account.rs`](../../../crates/rimz/src/agents/account.rs) reads `~/.codex/auth.json` directly (a cheap file read, no subprocess) for the logged-in-but-idle probe:

| Shape | Meaning |
| --- | --- |
| `OPENAI_API_KEY` present, non-empty | API-key login → **unmetered** by subscription windows; the provider dashboard uses transcript-derived API spend plus any display ceiling |
| `tokens.access_token` present | ChatGPT login → **metered** (plan tier filled by live app-server context or the OAuth usage response) |

The plan tier rides the app-server (`account/rateLimits/read` `plan_type`) and the OAuth usage response (`plan_type`) for idle or switched accounts. The semantics are in [adapter/codex.md → Account and balance](../../internals/agents/adapter/codex.md#account-and-balance).

[`oauth_usage.rs`](../../../crates/rimz/src/agents/codex/oauth_usage.rs) uses the same `tokens.access_token` for the direct account-usage probe. An API-key-only auth file has no OAuth endpoint and skips this path. When `tokens.account_id` is present, the request also sends `ChatGPT-Account-Id`; the same local `tokens.account_id` is the cache key used to detect account switches.

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
      "expires_at": "2026-07-06T12:00:00Z", // parsed for soonest_expiry
      "title": "Rate Limit Reset"        // present, ignored
    }
  ]
}
```

The usage response's `rate_limit_reset_credits` field remains ignored; the dedicated endpoint is the per-credit source and rides the standard OAuth usage cadence. A reset-credit fetch failure leaves the prior dashboard value in place when the primary usage fetch succeeds.
